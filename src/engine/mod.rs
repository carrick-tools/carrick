use crate::agent_service::{AgentService, RetryPolicy};
use crate::agents::file_orchestrator::FileOrchestrator;
use crate::agents::framework_guidance_agent::{FrameworkGuidanceAgent, ProtocolGuidance};
use crate::analyzer::{Analyzer, ApiEndpointDetails, builder::AnalyzerBuilder};
use crate::cloud_storage::{
    CACHE_DIR_ENV, CloudRepoData, CloudStorage, INLINE_PAYLOAD_LIMIT_BYTES, ManifestRole,
    ManifestTypeKind, ManifestTypeState, StorageError, TypeDegradation, TypeManifestEntry,
    UploadOutcome, get_current_commit_hash, mount_graph_to_api_details,
};
use crate::config::Config;
use crate::file_finder::find_service_files;
use crate::framework_detector::{DetectionResult, FrameworkDetector};
use crate::intent_generator::{IntentsInFlight, PreviousIntents, RunIntentMemo};
use crate::logging;
use crate::mount_graph::MountGraph;
use crate::multi_agent_orchestrator::MultiAgentOrchestrator;
use crate::operation::OperationKey;
use crate::packages::Packages;
use crate::parser::parse_file;
use crate::services::{
    TypeSidecar,
    type_sidecar::{InferKind, TypeResolutionResult},
};
use crate::signature_pass::populate_function_signatures;
use crate::type_manifest::{
    append_missing_aliases, build_manifest_type_alias_with_site_id, build_site_id,
    dts_alias_is_trivially_unknown, dts_defines_alias, is_http_method, is_producer_method,
    normalize_manifest_method, parse_file_location,
};
use crate::url_normalizer::UrlNormalizer;
use crate::utils::get_repository_name;
use crate::visitor::{FunctionDefinition, FunctionDefinitionExtractor, ImportSymbolExtractor};
pub use durability::ModelSetup;
use durability::ServiceAnalysis;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_visit::VisitWith;

pub mod durability;
pub(crate) mod served_paths;
pub(crate) mod type_compat_v2;
pub(crate) mod upload_boundary;

/// Current cache format version.
///
/// The incremental cache holds the model's RAW answer per file — exactly what
/// the analyzer returned, before the deterministic rows are emitted, before
/// the model's answer is joined onto them and before every pass over the join
/// (see `FileCentricAnalysisResult::raw_model_results`). The deterministic
/// layer runs on EVERY scan over EVERY discovered file, so a resolver fix, a
/// new deterministic source or a changed gate reaches an already-indexed repo
/// on its next push with zero model calls, and none of them bumps this
/// constant.
///
/// What DOES bump it: a change to the analyze-file prompt or to the response
/// schema. Those two decide what a cached answer IS, so a cache written under
/// the old one has to be discarded.
///
/// A change to how the deterministic layer FOLDS that answer onto its own rows
/// — which side's field wins at a span, what a kept prefix has to look like —
/// does not (carrick#697). The cache holds the answer, never the join, and the
/// join re-runs over the cached answer on every scan, so a corrected fold
/// reaches an already-indexed repo on its next push with zero model calls, the
/// same property that lets a resolver fix land without a bump.
///
/// The one join change that DOES bump is one that changes what the answer has
/// to carry to join at all — the `candidate_id` the prompt's hints hand the
/// model, which is the key every fold is looked up by. That is a prompt change,
/// and it bumps under the clause above. carrick#693 tracks the fact that none
/// of this is enforceable from inside this repo.
///
/// Invalidation is keyed by repo-relative path: a file goes back to the model
/// when its content on disk is not what the previous scan's commit holds (an
/// uncommitted edit counts, and an untracked file always does), or when the
/// cache holds no entry for it. A dirty run keeps the entries for files that
/// match its commit and drops only the rest (`stamp_tree_state`,
/// carrick#1079). Only a file the model was ASKED about has an
/// entry — a file phase 1 skipped (no candidate, unroutable protocol,
/// unparseable) and a file whose call failed are both absent. That absence is
/// deliberate and is what replaces the old "bump the version" escape hatch:
/// a skipped file is re-examined by phase 1 on every scan at zero model cost,
/// and is dispatched the first time an improved scanner raises a candidate for
/// it, without this constant moving and without re-analysing the repo. The
/// price is that "the model has nothing to say about this file" is never
/// cached, so a file that IS dispatched and answers with nothing is asked
/// again after any change to it — the same as today.
///
/// 21: the cache switched from the joined result to the model's raw answer,
/// and from keying every discovered file to keying only the files the model
/// was asked about. A v20 cache holds rows the deterministic layer stated,
/// which this version states again — replaying one would duplicate them and
/// pin the file to the resolver of the scan that wrote it.
///
/// 22: the extraction schema gained the body-dispatch discriminator
/// (carrick#831) — `dispatch` on an operation row and on a call row, and
/// `dispatch_tables` beside them. A v21 cache holds the model's answers from
/// before the field was asked for, so replaying one states a plain route for
/// every dispatching handler in the repo and the change is invisible on every
/// incremental scan until the files happen to move.
pub(crate) const CACHE_VERSION: u32 = 22;

// Type aliases to reduce complexity
/// What discovery reads from a service before any analysis runs.
#[derive(Debug)]
struct FileDiscovery {
    files: Vec<PathBuf>,
    import_facts: crate::framework_detector::ImportSample,
    function_definitions: HashMap<String, FunctionDefinition>,
    repo_name: String,
    /// What each function's calls send is composed from these over the call
    /// graph (carrick#1555), once the service's library semantics are
    /// verified ([`summarize_requests`], carrick#1564).
    request_inputs: crate::request_summary::RequestSummaryInputs,
}

type FileDiscoveryResult = Result<FileDiscovery, Box<dyn std::error::Error>>;

/// Determine if we should upload data based on GitHub context
/// Only upload on main/master branch, not on PRs
fn should_upload_data() -> bool {
    // Eval runs (`CARRICK_OUTPUT_JSON`) are read-only benchmarks against throwaway
    // fixtures. Never upload, or a dispatch on main would pollute the real cloud
    // index with fixture "services". This is the upstream half of eval mode's
    // no-side-effects guarantee; the JSON output branch skips the markdown
    // report + PR comment downstream.
    //
    // This branch returning first is what leaves a named cache directory empty
    // without saying so; `suppressed_upload_warning` is where the run says it
    // (carrick#966).
    if env::var("CARRICK_OUTPUT_JSON").is_ok() {
        return false;
    }

    // LocalDirStorage (the offline cross-repo eval harness, Phase A) writes
    // CloudRepoData to a local cache dir, never the real cloud — so the
    // PR/branch anti-pollution guards below do not apply. Without this, a CI
    // run (GITHUB_EVENT_NAME=pull_request) skips the upload and Phase A
    // persists nothing. Phase B sets CARRICK_OUTPUT_JSON and returns above.
    if env::var("CARRICK_LOCAL_STORAGE_DIR").is_ok() {
        return true;
    }

    // Check if we're in a pull request
    if let Ok(event_name) = env::var("GITHUB_EVENT_NAME")
        && event_name == "pull_request"
    {
        return false;
    }

    // Check if we're on a feature branch (not main/master)
    if let Ok(ref_name) = env::var("GITHUB_REF") {
        // GITHUB_REF format: refs/heads/branch-name or refs/pull/123/merge
        if ref_name.starts_with("refs/pull/") {
            return false;
        }

        if let Some(branch) = ref_name.strip_prefix("refs/heads/") {
            // Only upload for main/master branches
            return branch == "main" || branch == "master";
        }
    }

    // If we can't determine the context, default to upload (for local testing)
    // You might want to change this to false for stricter behavior
    true
}

/// What a run is told when it names a local cache directory and then
/// suppresses every write into it (carrick#966).
///
/// `CARRICK_OUTPUT_JSON` returns false from [`should_upload_data`] before the
/// `CARRICK_LOCAL_STORAGE_DIR` branch is reached. That is right for the phases
/// that READ the cache and print a projection — the cross-repo join, and every
/// hermetic fixture harness that uses the directory as a no-cloud sink — so
/// the combination is refused nowhere. It is wrong, and silently so, for a
/// pass meant to FILL the cache: it analyses every service, prints a complete
/// report, exits 0, and writes no blob. An empty cache directory is
/// indistinguishable from one that was never filled, so the phase that reads
/// it back finds nothing and the pass — a paid one, on a live corpus — is
/// lost.
///
/// Pure, so the wording is tested without touching the process environment.
fn suppressed_upload_warning(output_json: bool, local_dir: Option<&str>) -> Option<String> {
    let dir = local_dir?;
    if !output_json {
        return None;
    }
    Some(format!(
        "CARRICK_OUTPUT_JSON is set, so this run uploads nothing: {dir} \
         ({CACHE_DIR_ENV}) is left exactly as it was and no blob is written to \
         it. A read-only pass wants that; a pass meant to FILL that cache must \
         run with CARRICK_OUTPUT_JSON unset."
    ))
}

/// The PR number for a `pull_request` run, or None on push/dispatch/local
/// runs. GitHub sets GITHUB_REF to `refs/pull/<n>/merge` (or `/head`) on PRs;
/// returning None on any other ref is exactly the "only post on PR runs" gate.
fn pr_number_from_env() -> Option<u64> {
    let ref_name = env::var("GITHUB_REF").ok()?;
    let rest = ref_name.strip_prefix("refs/pull/")?;
    rest.split('/').next()?.parse::<u64>().ok()
}

/// This run's GitHub Actions run id (`GITHUB_RUN_ID`), or None if unset. The
/// cloud records it against the PR so a later sibling main change can re-run
/// this exact workflow run and refresh the comment.
fn run_id_from_env() -> Option<String> {
    env::var("GITHUB_RUN_ID").ok().filter(|id| !id.is_empty())
}

/// `pull_request.head.sha` from the GITHUB_EVENT_PATH event payload, or None
/// on any failure (missing env, unreadable file, unexpected JSON). The cloud
/// needs the head SHA to attach a check run; a merge-ref SHA from GITHUB_SHA
/// would pin the check to a commit that isn't on the PR branch.
fn head_sha_from_event() -> Option<String> {
    let path = env::var("GITHUB_EVENT_PATH").ok()?;
    let contents = std::fs::read_to_string(path).ok()?;
    let event: serde_json::Value = serde_json::from_str(&contents).ok()?;
    event
        .get("pull_request")?
        .get("head")?
        .get("sha")?
        .as_str()
        .map(str::to_string)
}

/// `pull_request.base.sha` from the GITHUB_EVENT_PATH event payload, or None
/// on any failure.
fn base_sha_from_event() -> Option<String> {
    let path = env::var("GITHUB_EVENT_PATH").ok()?;
    let contents = std::fs::read_to_string(path).ok()?;
    let event: serde_json::Value = serde_json::from_str(&contents).ok()?;
    event
        .get("pull_request")?
        .get("base")?
        .get("sha")?
        .as_str()
        .map(str::to_string)
}

/// The commit this PR run's tree is based on (carrick-cloud#1408): the first
/// parent of the merge commit a `pull_request` run checks out, which is what
/// is on disk, else the event's `pull_request.base.sha`.
fn pr_base_commit(repo_path: &str) -> Option<String> {
    let parents = crate::git_state::head_parents(repo_path).unwrap_or_default();
    if let [base, head] = parents.as_slice()
        && head_sha_from_event().is_none_or(|event_head| event_head == *head)
    {
        return Some(base.clone());
    }
    base_sha_from_event()
}

/// What the files this PR leaves alone showed about main's copy, as the middle
/// clause of the line that says whether the run compares with it.
fn untouched_reading_clause(reading: &crate::pr_baseline::UntouchedReading) -> String {
    use crate::pr_baseline::UntouchedReading;
    match reading {
        UntouchedReading::Alike { files, .. } => {
            format!("its rows for the {files} file(s) this PR leaves alone match this run's")
        }
        UntouchedReading::Differ {
            service,
            file,
            part,
        } => {
            let at = match (file.is_empty(), service) {
                (true, None) => String::new(),
                (true, Some(service)) => format!(" in {service}"),
                (false, None) => format!(" for {file}"),
                (false, Some(service)) => format!(" for {file} in {service}"),
            };
            format!("its {part}{at} differ from this run's")
        }
        UntouchedReading::NothingToCompare => {
            "this PR leaves no file with rows unchanged to compare it on".to_string()
        }
        UntouchedReading::Unknown(reason) => reason.clone(),
    }
}

/// Whether main's copy of this repo is main as this PR's base has it
/// (carrick-cloud#1408), with the reason logged either way.
///
/// A copy at another commit is stale only when this clone holds that commit
/// and it differs from the base under the scanned path: main moved after its
/// last index, so a finding main's copy lacks may be main's rather than the
/// PR's. A commit this clone does not hold (the squash-merged branch head a
/// laptop index scanned, a shallow clone) cannot be compared, and is logged.
///
/// A copy another scanner version wrote is compared with when that scanner
/// read the files this PR left alone as this run did (carrick#1530).
/// `as_uploaded` is this run's services in the form main's copy was stored in.
/// An operation of one of this repo's services: in a monorepo an endpoint
/// newly added to one service still counts as new even if a sibling service
/// already exposes the same route.
type ServiceEndpointKey = (Option<String>, crate::operation::OperationKey);

/// What a PR's endpoint delta is compared with (carrick#1712).
#[derive(Debug, PartialEq, Eq)]
enum DeltaBaseline {
    /// No stored index of this repo: nothing to compare with.
    Absent,
    /// A stored index another scanner version wrote, or one that did not say
    /// which: a row this run states and it did not may be the scanner's
    /// change rather than the PR's, so no delta is shown. Once main is
    /// scanned by this version, the versions agree and the delta returns.
    /// Holds the versions it names, for the log.
    OtherScanner(Vec<String>),
    /// The stored index's operations, by service and key.
    Keys(HashSet<ServiceEndpointKey>),
}

impl DeltaBaseline {
    /// The baseline `stored` (this repo's stored services) gives a run of
    /// `scanner`.
    fn of<'a>(stored: impl IntoIterator<Item = &'a CloudRepoData>, scanner: &str) -> Self {
        let stored: Vec<&CloudRepoData> = stored.into_iter().collect();
        if stored.is_empty() {
            return Self::Absent;
        }
        if stored
            .iter()
            .any(|repo| repo.scanner_version.as_deref() != Some(scanner))
        {
            let mut versions: Vec<String> = stored
                .iter()
                .map(|repo| {
                    repo.scanner_version
                        .clone()
                        .unwrap_or_else(|| "of unknown version".to_string())
                })
                .collect();
            versions.sort();
            versions.dedup();
            return Self::OtherScanner(versions);
        }
        Self::Keys(
            stored
                .iter()
                .flat_map(|repo| {
                    repo.endpoints
                        .iter()
                        .map(|e| (repo.service_name.clone(), e.key.clone()))
                })
                .collect(),
        )
    }
}

/// The operations `current` states that `previous` lacks, and those
/// `previous` states that `current` no longer does, each sorted by (method,
/// path, service) so the output is deterministic even when two services add
/// or drop the same operation.
fn endpoint_delta(
    previous: &HashSet<ServiceEndpointKey>,
    current: &[CloudRepoData],
) -> crate::findings::PrDelta {
    let endpoint_ref = |service: &Option<String>, key: &crate::operation::OperationKey| {
        let (label, name) = key.display_labels();
        crate::findings::EndpointRef {
            method: label,
            path: name,
            service: service.clone(),
        }
    };
    let sort_refs = |refs: &mut Vec<crate::findings::EndpointRef>| {
        refs.sort_by(|a, b| {
            (&a.method, &a.path, &a.service).cmp(&(&b.method, &b.path, &b.service))
        });
    };
    let mut current_keys = HashSet::new();
    let mut new_endpoints = Vec::new();
    let mut seen = HashSet::new();
    for service_data in current {
        for endpoint in &service_data.endpoints {
            let id = (service_data.service_name.clone(), endpoint.key.clone());
            current_keys.insert(id.clone());
            if !previous.contains(&id) && seen.insert(id) {
                new_endpoints.push(endpoint_ref(&service_data.service_name, &endpoint.key));
            }
        }
    }
    let mut removed_endpoints: Vec<crate::findings::EndpointRef> = previous
        .iter()
        .filter(|id| !current_keys.contains(*id))
        .map(|(service, key)| endpoint_ref(service, key))
        .collect();
    sort_refs(&mut new_endpoints);
    sort_refs(&mut removed_endpoints);
    crate::findings::PrDelta {
        new_endpoints,
        removed_endpoints,
    }
}

fn main_copy_against_base(
    repo_path: &str,
    main_self: &[CloudRepoData],
    as_uploaded: &[CloudRepoData],
) -> crate::pr_baseline::MainCopy {
    let base = pr_base_commit(repo_path);
    let mut reading = None;
    let copy = crate::pr_baseline::main_copy(
        main_self,
        base.as_deref(),
        |copy, base| crate::git_state::differs_under(repo_path, copy, base).ok(),
        || {
            let read = match base.as_deref() {
                None => crate::pr_baseline::UntouchedReading::Unknown(
                    "this run does not know the PR's base commit".to_string(),
                ),
                Some(base) => match crate::git_state::unchanged_since(repo_path, base) {
                    Ok(untouched) => {
                        crate::pr_baseline::untouched_reading(as_uploaded, main_self, &untouched)
                    }
                    Err(reason) => crate::pr_baseline::UntouchedReading::Unknown(format!(
                        "git could not compare this tree with the base: {}",
                        reason.trim()
                    )),
                },
            };
            let alike = read.alike();
            reading = Some(read);
            alike
        },
    );
    let scanned: Vec<String> = main_self
        .iter()
        .map(|repo| {
            format!(
                "{} by Carrick {}{}",
                repo.commit_hash,
                repo.scanner_version
                    .as_deref()
                    .unwrap_or("of unknown version"),
                if repo.dirty == Some(true) {
                    " with uncommitted changes"
                } else {
                    ""
                }
            )
        })
        .collect();
    let base_named = base.as_deref().unwrap_or("unknown");
    if copy == crate::pr_baseline::MainCopy::Current
        && let Some(reading) = reading.as_ref().filter(|reading| reading.alike())
    {
        info!(
            "Main's index ({}) was written by another Carrick version than this run's ({}), \
             and {}, so the run compares with it",
            scanned.join(", "),
            env!("CARGO_PKG_VERSION"),
            untouched_reading_clause(reading)
        );
    }
    match copy {
        crate::pr_baseline::MainCopy::Stale => info!(
            "Main's index ({}) is not main as this PR's base ({base_named}) has it, so a \
             finding it lacks is posted as not compared",
            scanned.join(", ")
        ),
        crate::pr_baseline::MainCopy::OtherScanner => info!(
            "Main's index ({}) was written by another Carrick version than this run's ({}), \
             and {}, so a finding it lacks is posted as not compared",
            scanned.join(", "),
            env!("CARGO_PKG_VERSION"),
            reading.as_ref().map_or_else(
                || "nothing was compared".to_string(),
                untouched_reading_clause
            )
        ),
        crate::pr_baseline::MainCopy::Current
            if main_self
                .iter()
                .any(|repo| Some(repo.commit_hash.as_str()) != base.as_deref())
                && base.is_some() =>
        {
            info!(
                "Main's index ({}) is not at this PR's base ({base_named}), and this clone \
                 does not hold that commit to compare, so the run compares with it as main's \
                 latest index",
                scanned.join(", ")
            )
        }
        crate::pr_baseline::MainCopy::Current => {}
    }
    copy
}

/// The sidecar the signature pass is allowed to ask, which is none at all when
/// this run skips inference (`CARRICK_SKIP_SIGNATURES`).
///
/// Signatures are still composed from what the source annotates; what is
/// dropped is the round trip that fills the slots it does not. The re-check
/// behind an edit (carrick#1036) is the only caller that asks for this: it
/// reads no signature and has a ten-second budget for the whole answer.
fn signature_sidecar(sidecar: Option<&TypeSidecar>) -> Option<&TypeSidecar> {
    if crate::local_mode::skip_signature_inference() {
        return None;
    }
    sidecar
}

#[allow(dead_code)]
pub async fn run_analysis_engine<T: CloudStorage + Sync>(
    storage: T,
    repo_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    run_analysis_engine_with_sidecar(storage, repo_path, None, false).await
}

/// Run analysis engine with optional sidecar for type extraction.
///
/// Always attempts log upload before returning, including on the error path —
/// the failing runs are exactly the ones whose logs we need. The inner
/// pipeline lives in `run_analysis_engine_inner` so `?`-propagated errors
/// don't bypass the upload.
pub async fn run_analysis_engine_with_sidecar<T: CloudStorage + Sync>(
    storage: T,
    repo_path: &str,
    sidecar: Option<&TypeSidecar>,
    no_cache: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = run_analysis_engine_inner(&storage, repo_path, sidecar, no_cache).await;
    // The marker first, then the log: the marker is four short fields and the
    // log is up to five megabytes, and a run that is already failing may not
    // get to finish both.
    if let Err(error) = &result {
        report_scan_failure(&storage, repo_path, error.as_ref()).await;
    }
    upload_run_logs(&storage, repo_path).await;
    result
}

/// Tell the cloud this run died before it could upload, and where
/// (carrick#1063).
///
/// The stage comes from [`crate::scan_stage`], which every phase of the
/// pipeline moves forward as it starts; the reason is the error's own words,
/// redacted and cut. Best-effort in both directions — the storage decides
/// whether it has a slot to mark, and nothing here can change what the run
/// returns.
///
/// Only a run that ends through this function can say anything at all. A
/// process the OS kills — SIGKILL, the OOM killer, a laptop lid — sends
/// nothing, and the cloud's slot TTL is what covers that.
async fn report_scan_failure<T: CloudStorage + Sync>(
    storage: &T,
    repo_path: &str,
    error: &dyn std::error::Error,
) {
    let stage = crate::scan_stage::current();
    let redaction = logging::Redaction::for_run(Some(repo_path));
    storage
        .report_scan_failed(stage.as_str(), &fail_reason(&error.to_string(), &redaction))
        .await;
}

/// Tell the cloud a run died before `start-scan` opened a scan
/// (carrick#1096).
///
/// The caller decides that no scan was opened and passes the stage the run
/// had reached (`main` reads [`crate::scan_stage::current`]); this names the
/// rest the same way the fail marker does. `repo` comes from the function `RunContext` reads
/// it with, so the name the cloud is told is the name `start-scan` would have
/// been given. Best-effort: nothing here can change what the run returns.
pub async fn report_preflight_failure<T: CloudStorage + Sync>(
    storage: &T,
    repo_path: &str,
    stage: crate::scan_stage::Stage,
    error: &dyn std::error::Error,
) {
    let repo = crate::git_state::remote_name(std::path::Path::new(repo_path));
    storage
        .report_preflight_failed(
            repo.as_deref(),
            stage.as_str(),
            &fail_reason(
                &error.to_string(),
                &logging::Redaction::for_run(Some(repo_path)),
            ),
        )
        .await;
}

/// What a failure event's `reason` carries — `scan-failed` and
/// `preflight-failed` alike: the error's own words, run through the same
/// per-line rules as the uploaded run log (home directory to `~`, a line
/// naming a credential dropped whole), and cut to [`FAIL_REASON_LIMIT`]
/// characters.
///
/// A reason whose every line named a credential is replaced by a sentence
/// saying so, rather than sent empty: the cloud still learns that the run
/// died, and in which stage.
///
/// Characters, not bytes, so a multi-byte error message is cut on a boundary
/// rather than panicking on the way out of a run that is already failing.
/// The cut is marked, so a truncated reason does not read as a complete
/// sentence that happens to stop.
pub(crate) fn fail_reason(error: &str, redaction: &logging::Redaction) -> String {
    const ELLIPSIS: &str = "...";
    const WITHHELD: &str = "(the error named a credential, so its text was not sent)";
    let kept: Vec<String> = error
        .lines()
        .filter_map(|line| redaction.line(line))
        .collect();
    let redacted = if kept.is_empty() && !error.is_empty() {
        WITHHELD.to_string()
    } else {
        kept.join("\n")
    };
    if redacted.chars().count() <= FAIL_REASON_LIMIT {
        return redacted;
    }
    let kept: String = redacted
        .chars()
        .take(FAIL_REASON_LIMIT - ELLIPSIS.len())
        .collect();
    format!("{kept}{ELLIPSIS}")
}

/// The wire limit on a fail marker's `reason`.
const FAIL_REASON_LIMIT: usize = 500;

/// Say the scan has reached `stage`, and stop it here if this process has been
/// signalled (carrick#1387).
///
/// A phase boundary is where an interrupted run can be abandoned without
/// leaving half a pass behind, and the only place a CPU-bound stage gives the
/// signal to notice it: nothing inside a parse or an analysis pass awaits, so
/// the race in `main` is not polled for as long as one runs. Returning here
/// ends the run the way any failure does — the scan is marked failed with this
/// as the reason, the log is shipped, and `main` reads the recorded signal to
/// leave with its code.
///
/// The boundaries that state a stage but return nothing (the discovery pass,
/// the per-endpoint definitions) are left as they are: the next boundary after
/// them is where such a run stops.
fn enter_stage(stage: crate::scan_stage::Stage) -> Result<(), crate::shutdown::Interrupted> {
    crate::scan_stage::enter(stage);
    crate::shutdown::check()
}

async fn run_analysis_engine_inner<T: CloudStorage + Sync>(
    storage: &T,
    repo_path: &str,
    sidecar: Option<&TypeSidecar>,
    no_cache: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let should_upload = should_upload_data();
    // What every upload of this run passes through last (carrick#1204).
    let boundary = upload_boundary::UploadBoundary::for_scan(repo_path);
    debug!(upload = should_upload, "Running Carrick in CI mode");
    // The run's one ceiling on waiting out a refusing model (carrick#1126).
    crate::retry_budget::reset();

    // Said before the scan, not after it: the point is that a capture pass set
    // up this way costs a full pass and leaves nothing behind (carrick#966).
    // `warn!` reaches a local run's stderr; the annotation puts the same line
    // on the Actions step, where a green scan that did less than it was asked
    // to is otherwise invisible.
    if let Some(warning) = suppressed_upload_warning(
        env::var("CARRICK_OUTPUT_JSON").is_ok(),
        env::var(CACHE_DIR_ENV).ok().as_deref(),
    ) {
        warn!("{warning}");
        logging::annotate(logging::Annotation::Warning, &warning);
    }

    // 1. Open the run.
    //
    // What git says about the tree is computed once, here, because three
    // things need it and they must agree: the warning printed below, the
    // `dirty` claim `start-scan` is given, and the `dirty` field stamped on
    // every payload.
    let git_state = crate::git_state::inspect(repo_path);
    // Which files match HEAD before any of them is read, so an edit made while
    // the scan runs cannot slip an answer about uncommitted bytes into the
    // cache (see `stamp_tree_state`).
    let unchanged_at_start = crate::git_state::unchanged_since(repo_path, &git_state.commit).ok();
    let run_context = crate::cloud_storage::RunContext {
        repo_full_name: crate::git_state::remote_name(std::path::Path::new(repo_path)),
        commit: git_state.commit.clone(),
        dirty: git_state.dirty,
    };
    enter_stage(crate::scan_stage::Stage::Discovery)?;
    let sp = logging::spinner("Connecting to Carrick Cloud...");
    let run_start = storage
        .begin_run(&run_context)
        .await
        // Not always a transport problem: on the laptop path this is the gate,
        // and its refusals name their own cause. "Failed to connect" would
        // send the user to check their network for a scan that is simply
        // already running.
        .map_err(|e| format!("Carrick Cloud did not open this scan: {}", e))?;
    logging::finish_spinner(&sp, "Connected to Carrick Cloud");
    // Both said at the top, not the end.
    //
    // The git warning is a laptop concern: CI runs on a checkout git made, and
    // a pull_request run is deliberately at a merge commit that is not
    // origin/main, so warning there would be noise on every PR. Warn, never
    // refuse (David's ruling, 2026-09-11) — nothing here can stop the run,
    // and `dirty` rides the upload whatever the user does about it
    // (carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §8.4).
    if run_start.is_laptop() {
        for line in crate::git_state::warnings(&git_state) {
            warn!("{line}");
        }
    }
    // A run whose candidates will not be refreshed says so before it spends
    // fifteen minutes not refreshing them.
    if let Some(sentence) = &run_start.allowance_sentence {
        warn!("{sentence}");
    }

    // Dispatch, decided here because this is the first moment both halves of
    // the question are answerable: the indexer asked for it in the
    // environment, and `start-scan` has just said whether this cloud runs
    // analysis jobs (carrick#1229). A cloud that does not is not an error —
    // this run scans the way every run did before, and the user waits.
    if crate::analysis_channel::dispatch_requested() {
        if storage.accepts_analysis_job() {
            crate::analysis_channel::begin_dispatch();
        } else {
            info!(
                "Carrick Cloud is not running analysis jobs yet, so this scan analyses the repo \
                 here and now."
            );
            // And on the marker channel, because the indexer swallows this
            // process's stderr and `--dispatch` would otherwise look like a
            // flag that did nothing (carrick#1251).
            crate::progress::report_not_dispatched(crate::progress::NotDispatched::CloudDeclined);
        }
    }

    // 2. Download all repos (moved earlier for incremental cache lookup)
    let sp = logging::spinner("Downloading cross-repo data...");
    let (all_repo_data, _repo_s3_urls) = storage
        .download_all_repo_data()
        .await
        .map_err(|e| format!("Failed to download cross-repo data: {}", e))?;

    // Local mode's join phase (carrick#708). Every service in the workspace
    // was already scanned into the cache dir, one repo at a time, so there is
    // nothing to analyse here: this run exists to join the blobs, run the type
    // check over them, and hand the result back. Terminal, like the eval JSON
    // branch below, and unreachable unless the indexer asked for it by naming
    // an output path.
    //
    // The scan target is still a real repo, because the sidecar initialises
    // against it and a type check with no sidecar produces no verdicts at all.
    if let Some(out_path) = env::var(crate::local_mode::JOIN_OUT_ENV)
        .ok()
        .map(PathBuf::from)
    {
        let sp = logging::spinner("Joining the workspace...");
        // What the check did rides back in the join file, because this
        // process's own warning is swallowed by the indexer that spawned it
        // (carrick#1490).
        let (analyzer, type_check) =
            build_cross_repo_analyzer(all_repo_data, Vec::new(), sidecar).await?;
        let results = analyzer.get_results();
        crate::local_mode::LocalJoin::from_results(&results, type_check).write(&out_path)?;
        logging::finish_spinner(
            &sp,
            &format!(
                "Joined {} operation(s), {} edge(s)",
                results.endpoints.len() + results.calls.len(),
                results.cross_repo_matches.len()
            ),
        );
        return Ok(());
    }

    // 3. Resolve the services declared for this repo (one for the common
    //    single-service case; one per directory for a monorepo carrick.json).
    let repo_name = get_repository_name(repo_path);
    let services = resolve_services(repo_path)?;
    let multi_service = services.len() > 1;
    if multi_service {
        info!("Resolved {} services in {}", services.len(), repo_name);
    }
    // What was downloaded is every OTHER repo in the project: a project whose
    // only repo is this one has nothing to fetch, and "from 0 repos" under a
    // green tick read as a download that had silently failed (carrick#1023
    // item 10).
    logging::finish_spinner(
        &sp,
        &if all_repo_data.is_empty() {
            "No sibling repos to download: this project holds only this one".to_string()
        } else {
            format!("Downloaded data from {} repos", all_repo_data.len())
        },
    );

    let local_previous = crate::local_mode::hosted::previous_data()?;

    // 4. Analyze each service (incremental per service where possible).
    let sp = logging::spinner("Analyzing repository...");
    // One workspace pass for the whole scan. Every service asks it the same
    // question about the same tree, and rebuilding it per service was the
    // largest fixed cost in the analysis phase (carrick#767).
    let mut workspace_scan = crate::external_call_candidates::WorkspaceScan::new();
    // Every schema file the repository holds, marked served or external, read
    // once for the whole scan: a document in one service is attributed against
    // the schemas every service serves (carrick#1134).
    let graphql_schemas = crate::graphql::SchemaCatalogue::build(
        Path::new(repo_path),
        &services
            .iter()
            .map(|service| crate::graphql::ServedSchemaSources {
                roots: service_graphql_roots(repo_path, service),
                declared: crate::graphql::resolve_declared_schemas(
                    Path::new(repo_path),
                    &service.graphql_schemas,
                )
                .files,
            })
            .collect::<Vec<_>>(),
    );
    // One intent memo for the whole scan, so a function several services hold
    // is described once (carrick#1080). The retry below reads it too.
    let run_intents = RunIntentMemo::default();
    let scan = ServiceScan {
        repo_path,
        sidecar,
        total: services.len(),
        run_intents: &run_intents,
        graphql_schemas: &graphql_schemas,
    };
    // Where each service's previous generation is read from: the laptop's
    // hosted snapshot when the indexer handed one in, otherwise the download.
    let previous_generations = local_previous.as_ref().unwrap_or(&all_repo_data);
    let upload_blocked = multi_service && !storage.supports_multi_service();
    let mut runs: Vec<ServiceRun> = Vec::with_capacity(services.len());
    for (index, service) in services.iter().enumerate() {
        // Incremental cache is per service: match on repo + service name so
        // editing one service does not invalidate the others.
        let previous_data = if no_cache {
            None
        } else {
            previous_generation(previous_generations, &repo_name, service)
        };
        match scan
            .analyze(
                index,
                service,
                previous_data.as_ref(),
                PreviousGeneration::Stored,
                &mut workspace_scan,
            )
            .await
        {
            Ok(run) => runs.push(run),
            Err(error) => {
                // Not a model failure — those defer, they do not reach here —
                // so something this run cannot work around. The services that
                // finished owe nothing, and they land before the run ends, so
                // the next scan replays them instead of paying for them again.
                // Verdict-less, as on the cross-repo failure path below, and
                // without closing the scan: the fail marker the caller sends
                // releases the slot and does not call this a finished index.
                logging::finish_spinner_warn(&sp, "Analysis stopped");
                if should_upload && !upload_blocked {
                    let finished: Vec<&CloudRepoData> = runs
                        .iter()
                        .filter(|run| run.owed.is_empty())
                        .map(|run| &run.data)
                        .collect();
                    if !finished.is_empty() {
                        warn!(
                            "Analysis stopped at service {}/{}; landing the {} service(s) that \
                             finished before it",
                            index + 1,
                            services.len(),
                            finished.len()
                        );
                        let payloads = upload_payloads_for(
                            storage,
                            repo_path,
                            &finished,
                            git_state.dirty,
                            unchanged_at_start.as_ref(),
                        );
                        upload_service_payloads(storage, &payloads, no_cache, false, &boundary)
                            .await;
                    }
                }
                return Err(error);
            }
        }
    }

    // 4a. A dispatched run ends here, and this is its whole product: every
    // prompt every service built, shipped as one job (carrick#1229).
    //
    // Nothing below this point has anything to work on — the model answers it
    // would join, cache, type-check and upload are what the job is for — and
    // the cloud is holding a slot for the job rather than for this process, so
    // the run exits 0 and the machine is free.
    //
    // A run asked to dispatch that built no prompt at all is not that run. It
    // has nothing to hand over, every service went through the ordinary phases
    // (see `analyze_files`), and it finishes here the way any other scan does:
    // it writes its index, closes its scan and leaves nobody waiting for
    // answers that were never asked for. It says so on the way past, which is
    // the one thing it used not to do (carrick#1251).
    match crate::analysis_channel::take() {
        Some(collected) if !collected.rows.is_empty() => {
            let submitted =
                dispatch_analysis_job(storage, collected, &run_context, repo_path).await?;
            crate::progress::report_dispatched(&submitted);
            logging::finish_spinner(
                &sp,
                &format!(
                    "Carrick Cloud is analysing {} file(s) of {}",
                    submitted.analyze_rows, submitted.repo
                ),
            );
            return Ok(());
        }
        // Asked to dispatch, collected nothing. The cloud that declined the
        // job never started a collector, so it is not this branch: it said so
        // where it declined.
        Some(_) => {
            info!(
                "Nothing in this repo needed the analyzer, so there was nothing to hand over: it \
                 is indexed here."
            );
            crate::progress::report_not_dispatched(
                crate::progress::NotDispatched::NothingToAnalyse,
            );
        }
        None => {}
    }

    // 4b. The work the run still owes gets one more try before it ends: a
    // deferred service's detection, the files and intents the model did not
    // answer for. After minutes, not
    // seconds, because what usually causes all of them is a shared model
    // quota that refills on that scale.
    let retrying: Vec<usize> = runs
        .iter()
        .enumerate()
        .filter(|(_, run)| run.owed.worth_retrying())
        .map(|(index, _)| index)
        .collect();
    let retry_budget_left = crate::retry_budget::remaining();
    if !retrying.is_empty() && !crate::local_mode::no_model() && retry_budget_left.is_zero() {
        // The run already spent its ceiling on waiting (carrick#1126): what is
        // owed stays pending and the run finishes now.
        warn!(
            "Not retrying {} service(s) in this run: it has spent its {}s retry budget ({}). \
             They stay pending for the next scan.",
            retrying.len(),
            crate::retry_budget::budget().as_secs(),
            crate::retry_budget::BUDGET_ENV
        );
    } else if !retrying.is_empty() && !crate::local_mode::no_model() {
        // The wait is charged to the run's budget and never outlasts it.
        let delay = durability::retry_delay(
            retrying
                .iter()
                .any(|index| runs[*index].owed.thins_the_index()),
        )
        .min(retry_budget_left);
        let names: Vec<String> = retrying
            .iter()
            .map(|index| format!("{} ({})", runs[*index].label, runs[*index].owed.describe()))
            .collect();
        durability::wait_with_progress(delay, |left| {
            logging::progress(
                &sp,
                &format!(
                    "{} service(s) still owe model work: {}. Retrying them once in {}s",
                    retrying.len(),
                    names.join(", "),
                    left.as_secs()
                ),
            );
        })
        .await;
        for index in retrying {
            let service = &services[index];
            // Whatever this service lost is recorded again if it is lost again.
            crate::scan_health::forget_service_losses(service.service_name.as_deref());
            // Its own blob from a minute ago is the previous generation: every
            // answer it holds replays, and only what it lacks is asked for.
            let this_run = runs[index].data.clone();
            match scan
                .analyze(
                    index,
                    service,
                    Some(&this_run),
                    PreviousGeneration::ThisRun,
                    &mut workspace_scan,
                )
                .await
            {
                Ok(run) => runs[index] = run,
                Err(error) => {
                    warn!("Retrying service {} failed: {error}", runs[index].label);
                    runs[index].owed.retry_error = Some(error.to_string());
                }
            }
        }
    }

    // What the analysis actually achieved decides what the spinner is allowed
    // to claim. A run that lost files to failed analyzer calls indexed less
    // than it was asked to, and a tick next to "Analyzed" is the first half of
    // reporting success on a partial index (#461).
    //
    // A run that lost its type layer is the same shape of half-success: the
    // route surface survives, so nothing downstream fails, and only a gate
    // that asks for a type notices (carrick#748). It reaches the finish line
    // for the same reason, and the Action log for the only reason it can —
    // the Action tees this output and prints nothing of its own.
    let lost_files = crate::scan_health::lost_file_count();
    let types_lost = crate::scan_health::types_summary_line();
    let mut headline = format!("Analyzed {} ({} service(s))", repo_name, services.len());
    if lost_files > 0 {
        headline.push_str(&format!(
            "; {} of {} files were not analysed",
            lost_files,
            crate::scan_health::attempted_count()
        ));
    }
    if let Some(ref types_lost) = types_lost {
        headline.push_str(&format!("; {}", types_lost));
    }
    let owing = runs.iter().filter(|run| !run.owed.is_empty()).count();
    if owing > 0 {
        headline.push_str(&format!("; {owing} service(s) pending model analysis"));
    }
    if lost_files > 0 || types_lost.is_some() || owing > 0 {
        logging::finish_spinner_warn(&sp, &headline);
    } else {
        logging::finish_spinner(&sp, &headline);
    }
    if let Some(ref types_lost) = types_lost {
        warn!("{}", types_lost);
        logging::annotate(
            logging::Annotation::Warning,
            &format!(
                "Carrick indexed {} without types: {}",
                repo_name, types_lost
            ),
        );
    }

    // A budget that refused to answer is not a loss: the model was never
    // asked, so there is nothing to re-run and nothing to protect the index
    // from. Reported on its own line (carrick#555).
    if let Some(line) = crate::scan_health::not_refreshed_line() {
        warn!("{line}");
        logging::annotate(logging::Annotation::Warning, &line);
    }
    // Rows left out on purpose (carrick#1513): a count, so a reader who
    // expected them knows they were dropped rather than missed.
    if let Some(line) = crate::scan_health::in_process_pubsub_line() {
        crate::progress::announce(&line);
    }
    if let Some(summary) = crate::scan_health::summary_line() {
        warn!("{summary}");
    }

    // 4c. Which services land, decided one service at a time.
    //
    // A service that owes model work and already has an index is held back,
    // so the index stays stale rather than becoming thinner (#461): this used
    // to be one run-wide gate that aborted every service's upload for one
    // service's lost file.
    // A service with no index yet lands whatever it holds — there is nothing
    // to protect, and on the laptop path the cloud's partial rule decides
    // with the service's own unanalysed-file list. The lost files are absent
    // from the cache, so the next run re-analyses exactly them.
    //
    // `CARRICK_ALLOW_PARTIAL_ANALYSIS` still lands a held-back service; the
    // loss is reported either way.
    let allow_partial = crate::scan_health::allow_partial_from_env();
    let laptop = run_start.is_laptop();
    let authorized_basename = run_context
        .repo_full_name
        .as_deref()
        .and_then(|full| full.rsplit('/').next())
        .unwrap_or(&repo_name)
        .to_string();
    for run in &mut runs {
        let has_index = match run_start.indexed_services.as_deref() {
            Some(indexed) => {
                let slug = crate::cloud_storage::indexed_service_slug(
                    run.data.service_name.as_deref(),
                    &authorized_basename,
                );
                indexed.contains(&slug)
            }
            // CI asks nobody. What it downloaded is what the index holds.
            None => all_repo_data.iter().any(|repo| {
                repo.repo_name == repo_name && repo.service_name == run.data.service_name
            }),
        };
        run.held_back = durability::holds_back(&run.owed, has_index, laptop, allow_partial);
        if run.held_back {
            warn!(
                "Not uploading {}: it already has an index and this run still owes it model \
                 work ({}), so uploading would replace that index with a thinner one. Set {} to \
                 upload it anyway.",
                run.label,
                run.owed.describe(),
                crate::scan_health::ALLOW_PARTIAL_ENV
            );
        }
    }
    // Whether this run leaves a service short of a complete index. A missing
    // function description alone does not: it never held an index back.
    let incomplete = runs.iter().any(|run| run.owed.thins_the_index());

    // The data every later stage reads: the join, the PR delta, the eval
    // projection. A held-back service is represented by the generation the
    // index still serves, when there is one, so the join is computed against
    // what consumers will actually read.
    let current_services_data: Vec<CloudRepoData> = runs
        .iter()
        .map(|run| {
            if !run.held_back {
                return run.data.clone();
            }
            match previous_generations.iter().find(|repo| {
                repo.repo_name == repo_name && repo.service_name == run.data.service_name
            }) {
                Some(served) => {
                    if should_upload {
                        storage.keep_served_generation(served);
                    }
                    served.clone()
                }
                None => run.data.clone(),
            }
        })
        .collect();
    // Decided here, while every service's rows and dependency facts are still
    // in hand, and printed with the report (carrick#1099). Index for index
    // with `services`, as `current_services_data` is.
    let mut graphql_notices = graphql_schema_notices(repo_path, &services, &current_services_data);
    graphql_notices.hints.extend(graphql_schemas.notices());

    // 5. Prepare each service's upload payload, but DEFER the actual upload
    //    until after cross-repo analysis (step 6) so every payload can carry the
    //    per-pair type-compat verdicts that analysis computes (#351). Every
    //    payload is materialised now (cheap clones), so a serialization problem
    //    can't surface halfway through a multi-service upload.
    //
    //    The production index keys on (workspace, project, repo) only, so it
    //    cannot yet hold more than one service per repo — a multi-service upload
    //    would clobber. Gate it on the backend advertising support; cross-repo
    //    analysis below still runs locally regardless. `None` = do not upload
    //    (PR/branch mode, or an unsupported multi-service repo).
    enter_stage(crate::scan_stage::Stage::BlobBuild)?;
    let upload_payloads: Option<Vec<CloudRepoData>> = if should_upload {
        if upload_blocked {
            warn!(
                "Skipping index upload: {} services declared but the cloud key has no \
                 service discriminator yet, so uploads would overwrite each other. \
                 Cross-repo analysis still runs locally.",
                services.len()
            );
            None
        } else {
            let landing: Vec<&CloudRepoData> = runs
                .iter()
                .filter(|run| !run.held_back)
                .map(|run| &run.data)
                .collect();
            Some(upload_payloads_for(
                storage,
                repo_path,
                &landing,
                git_state.dirty,
                unchanged_at_start.as_ref(),
            ))
        }
    } else {
        debug!("Skipping upload (PR/branch mode)");
        None
    };

    // On a PR run, read what this repo's last uploaded index (main) says
    // before it is removed below, so the delta can surface what this change
    // added and removed relative to it. `had_prior_index` is tracked
    // separately so a prior scan that indexed zero endpoints still counts as
    // a baseline, rather than being conflated with a first-ever scan where
    // "new" is meaningless. On non-PR runs the capture is skipped entirely,
    // since the block is suppressed there anyway.
    let is_pr_run = pr_number_from_env().is_some();
    let (had_prior_index, delta_baseline) = if is_pr_run {
        let stored = all_repo_data
            .iter()
            .filter(|repo| repo.repo_name == repo_name);
        (
            all_repo_data.iter().any(|repo| repo.repo_name == repo_name),
            DeltaBaseline::of(stored, env!("CARGO_PKG_VERSION")),
        )
    } else {
        (false, DeltaBaseline::Absent)
    };

    // What the analysis phase put on the wire, once, for the whole scan. A
    // per-service count says which service made its calls; this says whether
    // the scan made any at all — the question a boundary reading "0 file(s)
    // sent to the analyzer" beside thousands of cloud-side invocations cannot
    // answer (carrick#767).
    info!(
        "Analysis issued {} cloud request(s): {}",
        crate::agent_service::request_counts()
            .values()
            .sum::<usize>(),
        crate::agent_service::requests_between(
            &std::collections::BTreeMap::new(),
            &crate::agent_service::request_counts(),
        ),
    );

    // 6. Cross-repo analysis (reuse already-downloaded data).
    // Remove this repo's downloaded copies so the freshly-analyzed services
    // are the ones used.
    //
    // On a PR run with a prior index, main's copy is kept aside instead of
    // dropped: the PR comment fails only on findings the PR introduced, and
    // matching main's copy against the same peers is how the run tells which
    // those are (carrick-cloud#1369).
    let (main_self_data, peers): (Vec<CloudRepoData>, Vec<CloudRepoData>) = all_repo_data
        .into_iter()
        .partition(|repo| repo.repo_name == repo_name);
    let all_repo_data = peers;
    let main_baseline_input =
        (is_pr_run && had_prior_index && !main_self_data.is_empty()).then(|| {
            // Main's copy passed the upload boundary, which replaces machine
            // paths with placeholders; this run's services are compared in
            // the same form.
            let as_uploaded: Vec<CloudRepoData> = current_services_data
                .iter()
                .map(|data| {
                    let id = data.service_name.as_deref().unwrap_or(&data.repo_name);
                    boundary.scrub(data, id).unwrap_or_else(|| data.clone())
                })
                .collect();
            let surface_unchanged = crate::pr_baseline::same_surface(&as_uploaded, &main_self_data);
            let copy = main_copy_against_base(repo_path, &main_self_data, &as_uploaded);
            MainBaselineInput {
                // The peers are only needed when there is something to run.
                peers: if surface_unchanged {
                    Vec::new()
                } else {
                    all_repo_data.clone()
                },
                main_self: main_self_data,
                surface_unchanged,
                copy,
            }
        });

    // Peer repos and local service count describe the project topology, which
    // the formatter uses to frame findings (single repo / monorepo / poly-repo)
    // and to decide whether connectivity findings are conclusive. Captured
    // before `all_repo_data` is moved into the analyzer below.
    let peer_repo_count = all_repo_data.len();
    let local_service_count = services.len();

    debug!(
        "Cross-repo analysis with {} other repos + {} local service(s)",
        peer_repo_count, local_service_count
    );

    // On a PR run with a prior index this scanner version wrote, surface
    // what this change added and removed: operations in the freshly-analyzed
    // services that the previous (last-uploaded) index didn't have, and
    // previously-indexed operations that no longer exist. Because the
    // baseline is the last uploaded index, this can include an operation that
    // landed on main since its last scan rather than in this PR. Computed
    // before `current_services_data` is moved into the analyzer.
    let pr_delta = match delta_baseline {
        DeltaBaseline::Keys(previous) => Some(endpoint_delta(&previous, &current_services_data)),
        DeltaBaseline::OtherScanner(versions) => {
            info!(
                "Main's index was written by Carrick {} and this run is Carrick {}, so the PR \
                 comment lists no endpoint changes until main is scanned again",
                versions.join(", "),
                env!("CARGO_PKG_VERSION")
            );
            None
        }
        DeltaBaseline::Absent => None,
    };

    // Collect the merged type manifest before `all_repo_data` /
    // `current_services_data` are moved into the analyzer. The eval projection
    // joins these to each op (keyed by OperationKey) for the type-resolution
    // metrics; nothing else reads it. Cheap clone, only materialised for the
    // eval path (it's just a flatten of each repo's already-built manifest).
    let eval_type_manifest: Vec<TypeManifestEntry> = if std::env::var("CARRICK_OUTPUT_JSON").is_ok()
    {
        all_repo_data
            .iter()
            .chain(current_services_data.iter())
            .filter_map(|repo| repo.type_manifest.as_ref())
            .flat_map(|entries| entries.iter().cloned())
            .collect()
    } else {
        Vec::new()
    };

    // Type extraction is a per-service stage that can fail on its own (a dead
    // sidecar, a program too large for its heap) while every other stage
    // succeeds. Collected here, before the blobs move into the analyzer, so a
    // run that lost its types says so in the report and the PR comment
    // instead of looking clean (carrick#535).
    let type_degradations = degraded_type_findings(&current_services_data);

    // Same reason as above: project what the SDK-edge join reads before the
    // blobs move into the analyzer. Its other input — the `CrossRepoMatch`
    // edges — only exists after the analysis runs, so the two halves cannot be
    // gathered in one place. Peers are every blob in the run (any of them can
    // be the publisher); consumers are the current services only, because an
    // edge is stored on the consumer's blob and this run writes only its own.
    let sdk_join_input = crate::sdk_edges::SdkJoinInput::collect(
        all_repo_data.iter().chain(current_services_data.iter()),
        current_services_data.iter(),
    );

    // Each local service's boundary (carrick#705), taken for the same reason:
    // the blobs are about to move into the analyzer, and the SDK half of the
    // boundary is only known once the join below has run. Keyed by the same
    // `service_name ?? repo_name` id every other cross-repo surface uses.
    let mut boundaries: Vec<(String, crate::boundary::ServiceBoundary)> = current_services_data
        .iter()
        .filter_map(|data| {
            let id = data
                .service_name
                .clone()
                .unwrap_or_else(|| data.repo_name.clone());
            data.boundary.clone().map(|boundary| (id, boundary))
        })
        .collect();

    // Handlers that switch on a request field with no `operations` block yet
    // (carrick#831), taken here for the same reason as the two above: the
    // blobs are about to move into the analyzer. Every blob in the run is
    // read, because the consumers that name a value are in the peers and the
    // handler that answers them is in the current services (or the reverse on
    // a peer's own run).
    let dispatch_advisories = crate::dispatch::dispatch_operation_findings(
        &all_repo_data
            .iter()
            .chain(current_services_data.iter())
            .cloned()
            .collect::<Vec<_>>(),
    );

    enter_stage(crate::scan_stage::Stage::CrossRepoCheck)?;
    let sp = logging::spinner("Running cross-repo analysis...");
    let analyzer = match build_cross_repo_analyzer(all_repo_data, current_services_data, sidecar)
        .await
    {
        Ok((analyzer, type_check)) => {
            // For the indexer that spawned this scan, which swallows the
            // warning a skipped check logs (carrick#1490). A no-op anywhere
            // else.
            crate::progress::report_type_check(&type_check);
            analyzer
        }
        Err(e) => {
            // Cross-repo analysis (which is what runs the type check) failed. Close
            // the spinner with a warning first so the upload's own spinner
            // and log lines don't interleave with an unfinished one in
            // non-TTY CI logs, then preserve the prior behavior where the
            // per-repo index upload happened BEFORE cross-repo analysis:
            // still upload this run's data — verdict-less — so the index
            // stays fresh, then propagate the failure.
            logging::finish_spinner_warn(&sp, "Cross-repo analysis failed");
            if let Some(payloads) = &upload_payloads {
                // Whatever the upload could not confirm has already been
                // named in its own summary line and annotation; the error
                // this path returns is the analysis failure that brought it
                // here, which is the one worth raising (carrick#1067).
                upload_service_payloads(storage, payloads, no_cache, !incomplete, &boundary).await;
            }
            return Err(e);
        }
    };
    logging::finish_spinner(&sp, "Cross-repo analysis complete");

    let mut results = analyzer.get_results();

    // A degraded service means no type verdict for anything it owns, so the
    // whole run's type layer is untrustworthy, not just that service's.
    let has_types = type_degradations.is_empty();
    results.findings.extend(type_degradations);

    // Advisory: the index has one operation where the source has several, and
    // the block that fixes it is a paste away (carrick#831).
    results.findings.extend(dispatch_advisories);

    // 6a. Resolve this run's SDK-mediated consumer edges: consumer candidate →
    //     the publishing repo's exported member → the producer endpoint that
    //     member's own outbound call already matched. Nothing new is matched
    //     here (see `crate::sdk_edges`); the edges the analyzer just produced
    //     are read, and the result is its own relationship, never folded back
    //     into `endpoints`/`calls`. Computed outside the upload branch because
    //     the PR comment renders them and a PR run never uploads.
    let sdk_join = crate::sdk_edges::join(
        &sdk_join_input,
        &results.cross_repo_matches,
        &analyzer.pair_directions(),
    );
    if !sdk_join.is_empty() {
        debug!(
            "SDK edges: {} resolved, {} unresolved group(s)",
            sdk_join.edges().len(),
            sdk_join.unresolved().len()
        );
    }
    results.sdk_edges = sdk_join.edges().to_vec();
    results.sdk_unresolved = sdk_join.unresolved();
    for (id, boundary) in &mut boundaries {
        boundary.fold_sdk_unresolved(&sdk_join.unresolved_for(id));
    }

    // An SDK-mediated break is a contract risk like any other, so it joins the
    // findings rather than living only in its own section (#525). The PR result
    // payload the cloud renders the comment from carries `findings` and not
    // `sdk_edges`, so this is what turns the consumer's PR red.
    results
        .findings
        .extend(crate::sdk_edges::type_mismatch_findings(&results.sdk_edges));

    // Eval harness output mode: emit a machine-readable projection of the
    // results and skip the human Markdown report + PR-comment relay. Consumed
    // by the offline scorer (Slice 1 of the evals plan). Deliberately terminal —
    // an eval run wants only the JSON, no upload or comment side effects. (Eval
    // mode never uploads, so `upload_payloads` is always `None` here.)
    if std::env::var("CARRICK_OUTPUT_JSON").is_ok() {
        let projection = crate::eval_output::EvalProjection::from_results(
            &results,
            &eval_type_manifest,
            &analyzer.call_raw_targets(),
            &analyzer.call_bases(),
            &analyzer.call_unfollowed_members(),
        );
        crate::outln!("{}", serde_json::to_string_pretty(&projection)?);
        return Ok(());
    }

    // 6b. Upload each service's data, now carrying the per-pair type-compat
    //     verdicts cross-repo analysis just computed for the edges this repo's
    //     calls consume (#351). Keyed by canonical pair identity so the cloud
    //     MCP `check_compatibility` tool can surface the real verdict instead of
    //     structural-matching-only. Absent for edges the check didn't evaluate,
    //     which the cloud reads as "not compared" (fail closed, #324).
    //
    //     A service whose index the upload could not confirm does not end the
    //     run here: the report below is about a scan that already happened, so
    //     it is printed, and the run exits non-zero naming them at the very end
    //     (carrick#1067).
    let mut unconfirmed_uploads: Vec<UnconfirmedUpload> = Vec::new();
    // The services left pending, the same set `incomplete` reads, and whether
    // this run's last write closed the scan (carrick-cloud#892). A budget
    // refusal is in it: a first index whose ceiling is spent stays open, so a
    // raised per-workspace ceiling still governs its re-run.
    let first_index_open: Vec<String> = runs
        .iter()
        .filter(|run| run.owed.thins_the_index())
        .map(|run| run.label.clone())
        .collect();
    let mut closed_by_final_write = false;
    if let Some(mut payloads) = upload_payloads {
        crate::cloud_storage::attach_compat_verdicts(
            &mut payloads,
            &results.cross_repo_matches,
            &analyzer.pair_directions(),
        );
        // SDK edges ride along on the same terms as the verdicts: small,
        // consumer-side, canonical-keyed, and attached before the size guard.
        crate::sdk_edges::attach_sdk_edges(&mut payloads, &sdk_join);
        // The boundary rides on the same terms, and is attached here rather
        // than at scan time because its SDK half is a cross-repo fact
        // (carrick#705).
        for payload in &mut payloads {
            let id = payload
                .service_name
                .clone()
                .unwrap_or_else(|| payload.repo_name.clone());
            if let Some((_, boundary)) = boundaries.iter().find(|(known, _)| *known == id) {
                payload.boundary = Some(boundary.clone());
            }
        }
        // The size guard already ran once inside strip_ast_nodes, but the
        // verdicts were appended after it — re-apply so a payload that was
        // near the cap can't be re-inflated past it and 413 the upload
        // (verdicts are tiny; the caches are what gets dropped).
        let staging_available = storage.stages_oversized_payloads();
        for payload in &mut payloads {
            enforce_payload_size_limit(payload, staging_available);
        }
        // The cloud stamps a repo's first index finished on the write that
        // closes the scan, and the re-run that fills a pending service in
        // would then be metered as an ordinary scan (carrick-cloud#892). So a
        // run that leaves one pending closes with its last write only when
        // that write can name them to a cloud that reads the list; otherwise
        // no write is final and the fail marker after the summary closes it.
        let closes_run =
            first_index_open.is_empty() || storage.name_pending_on_final_write(&first_index_open);
        closed_by_final_write = closes_run && !payloads.is_empty();
        // The last boundary, and the one that matters most: a run somebody
        // stopped must not replace this repo's index with what it had got to
        // (carrick#1387). The salvage uploads on the failure paths above are
        // left as they are — each lands services that finished whole before
        // the run stopped, which is as true of an interruption as of any
        // other failure.
        enter_stage(crate::scan_stage::Stage::Upload)?;
        unconfirmed_uploads =
            upload_service_payloads(storage, &payloads, no_cache, closes_run, &boundary).await;
    }

    // Which of this PR's findings main already had (carrick-cloud#1369). Run
    // after this run's own analysis, which has taken the local consumers the
    // type check retypes: main's sources are not on disk, so its copy is
    // checked from its stored surface alone. The input exists only on a PR
    // run with a prior index.
    if let Some(input) = main_baseline_input {
        mark_findings_on_main(&mut results.findings, input, sidecar).await;
    } else if is_pr_run {
        // Nothing of this repo on main to compare with: say so on each
        // finding, rather than leave the cloud to read it as introduced
        // (carrick-cloud#1408).
        crate::pr_baseline::mark_all_unknown(
            &mut results.findings,
            crate::findings::OnMainUnknown::NoMainIndex,
        );
    }

    let topology = crate::findings::Topology {
        repo_name: repo_name.clone(),
        local_service_count,
        peer_repo_count,
    };

    // On pull_request runs we deliberately skip the index upload (see
    // should_upload_data — PR-branch data must not pollute the cross-repo
    // index), but we still relay the structured findings to the cloud, which
    // renders and posts (and updates in place on later pushes) a single PR
    // comment + check run via the GitHub App, gated on the project's
    // pr_comments_enabled toggle. Best-effort: a relay failure is logged,
    // never fatal. Assembled before `results` moves into the formatter.
    let pr_result = pr_number_from_env().map(|pr_number| crate::findings::PrResultPayload {
        repo: repo_name.clone(),
        pr_number,
        head_sha: head_sha_from_event(),
        run_id: run_id_from_env(),
        topology: topology.clone(),
        stats: crate::findings::ScanStats {
            endpoints: results.endpoints.len(),
            calls: results.calls.len(),
        },
        findings: results.findings.clone(),
        delta: pr_delta.clone(),
        verified: results
            .verified_endpoints
            .iter()
            .map(|entry| crate::findings::VerifiedEndpoint {
                method: entry.method.clone(),
                path: entry.path.clone(),
                provenance: entry.provenance,
                type_verdict: entry.type_verdict,
                producer_wider: entry.producer_wider,
            })
            .collect(),
        graphql: crate::findings::GraphqlStatus {
            libraries: results.detected_graphql_libraries.clone(),
            operations_indexed: results.graphql_operations_indexed,
        },
        has_types,
    });

    let formatted = crate::formatter::FormattedOutput::new(results, topology, pr_delta);
    formatted.print();

    // The last lines of the run: what it could not classify (carrick#705). An
    // answer that ends without its boundary reads as a complete one.
    print_boundaries(&boundaries);
    print_graphql_notices(&graphql_notices);

    if let Some(payload) = pr_result {
        // The payload as the cloud receives it. The terminal report renders the
        // findings without the fields a reader has to judge them by
        // (`edge_source`, `verdict_state`, carrick#727), and no storage backend
        // shows it, so an offline run has no way to see what a scan would post.
        match serde_json::to_string(&payload) {
            Ok(json) => debug!("PR result payload: {json}"),
            Err(error) => debug!("PR result payload could not be serialized: {error}"),
        }
        if let Err(e) = storage.post_pr_result(&payload).await {
            warn!("Failed to post PR result: {}", e);
        }
    }

    // What the run still owes, said last, where it is read. Only a service
    // whose owed work thins its index
    // changes how the run ends; a missing function description is listed and
    // nothing more.
    let owed: Vec<(String, durability::OwedWork)> = runs
        .iter()
        .filter(|run| !run.owed.is_empty())
        .map(|run| (run.label.clone(), run.owed.clone()))
        .collect();
    if !owed.is_empty() {
        let complete: Vec<String> = runs
            .iter()
            .filter(|run| run.owed.is_empty())
            .map(|run| run.label.clone())
            .collect();
        let summary = durability::pending_summary(&complete, &owed, laptop);
        warn!("{summary}");
        logging::annotate(logging::Annotation::Warning, &summary);
        crate::progress::report_pending(&summary);
    }
    if incomplete && should_upload {
        let reason: Vec<String> = owed
            .iter()
            .filter(|(_, work)| work.thins_the_index())
            .map(|(service, work)| format!("{service} ({})", work.describe()))
            .collect();
        let reason = format!("pending model analysis: {}", reason.join(", "));
        // A laptop scan whose last write could not close it (see step 6b: a
        // cloud that does not read `pending_services`, or nothing left to
        // upload) is closed here: the marker releases the slot and leaves the
        // repo's first index open. Either way the run succeeded at everything
        // it could do, so it exits 0 and the local index is built from what
        // landed.
        if !closed_by_final_write {
            storage
                .report_scan_failed(
                    crate::scan_stage::current().as_str(),
                    &fail_reason(&reason, &logging::Redaction::for_run(Some(repo_path))),
                )
                .await;
        }
        // CI has no slot and no local index. Its exit code is the only place
        // a stale service can show, as it was when one lost file failed the
        // whole run; now the rest of the run has landed first. The partial
        // opt-in keeps such a run green, as it always did.
        //
        // A service whose only debt is what a limit refused does not fail it:
        // hitting a limit never fails a scan (carrick-cloud#401). It is named
        // in the pending summary above, and held back when it has an index.
        let failing: Vec<String> = owed
            .iter()
            .filter(|(_, work)| work.thins_the_index() && !work.only_refused())
            .map(|(service, work)| format!("{service} ({})", work.describe()))
            .collect();
        if !laptop && !allow_partial && !failing.is_empty() {
            return Err(format!(
                "Pending model analysis: {}. The other services were uploaded.",
                failing.join(", ")
            )
            .into());
        }
    }

    // The last word of a run whose index is not what it should be. Everything
    // above happened; this is what makes the exit code say so (carrick#1067).
    if !unconfirmed_uploads.is_empty() {
        return Err(unconfirmed_upload_error(&unconfirmed_uploads).into());
    }

    // What this scan spent its wall clock on, for the build that started it.
    // Only a run that got this far: a scan that failed measured a wait nobody
    // should be told to expect again (carrick#1452).
    crate::scan_timing::report();
    Ok(())
}

/// Where one scan's services are analysed from: the pieces every service, and
/// every retry of one, reads the same.
struct ServiceScan<'a> {
    repo_path: &'a str,
    sidecar: Option<&'a TypeSidecar>,
    total: usize,
    run_intents: &'a RunIntentMemo,
    graphql_schemas: &'a crate::graphql::SchemaCatalogue,
}

/// One service's analysis in this run, and what it still owes.
struct ServiceRun {
    /// The name every log line and the summary use for it.
    label: String,
    data: CloudRepoData,
    owed: durability::OwedWork,
    /// Decided once every service has been analysed (step 4c).
    held_back: bool,
}

impl ServiceScan<'_> {
    /// Analyse one service and read back what it owes the model.
    ///
    /// `Err` is a failure that is not the model's: a manifest that cannot be
    /// read, a tree that cannot be walked. Every model failure is inside the
    /// returned [`durability::OwedWork`].
    async fn analyze(
        &self,
        index: usize,
        service: &Config,
        previous_data: Option<&CloudRepoData>,
        generation: PreviousGeneration,
        workspace: &mut crate::external_call_candidates::WorkspaceScan,
    ) -> Result<ServiceRun, Box<dyn std::error::Error>> {
        // One line per service, at info, before the work starts. A scan of a
        // large monorepo spends most of its wall clock inside this loop, and
        // without a mark per iteration a stall is a silence with nothing to
        // attribute it to: the 0.3.42 incident cost sixteen minutes between
        // two unrelated log lines, and reading which service it was in took
        // the whole investigation (carrick#748).
        let label = service.service_name.as_deref().unwrap_or("(root)");
        info!("Analyzing service {} ({}/{})", label, index + 1, self.total);
        // The same fact, for a parent process that is rendering a line rather
        // than reading a log (carrick#955).
        crate::progress::service_started(label, index + 1, self.total);
        // Every loss from here on is this service's.
        crate::scan_health::enter_service(service.service_name.as_deref());
        // And so is every prompt-lambda call: they all happen inside this
        // function, and each one carries this name so the cloud's logs can
        // say which tree of a monorepo spent the money (carrick#1221). The
        // guard is bound, not dropped on this line: the scope has to outlive
        // the analysis below and end with it, so that the cross-repo phase,
        // the upload and a service that failed mid-loop name nobody.
        let _service_scope = crate::current_service::enter(service.service_name.as_deref());
        let service_started = Instant::now();

        let packages = load_packages_for_service(self.repo_path, service)?;
        let packages_took = service_started.elapsed();

        // Scope the sidecar's type extraction to this service's directory/tsconfig.
        let sidecar_started = Instant::now();
        scope_sidecar_to_service(self.sidecar, self.repo_path, service);
        let sidecar_took = sidecar_started.elapsed();

        let analysis_started = Instant::now();
        let requests_before = crate::agent_service::request_counts();
        crate::phase_timing::start_service();
        let analysis = analyze_current_repo_incremental(
            self.repo_path,
            service,
            &packages,
            self.sidecar,
            previous_data,
            generation,
            workspace,
            self.run_intents,
            self.graphql_schemas,
        )
        .await?;

        // `analysis` used to be the whole phase and nothing said what was in
        // it (carrick#767). The breakdown after the colon adds up to it, and
        // the request counts say how many round trips this service made —
        // which is what distinguishes work that grew from waiting that grew.
        let totals = crate::phase_timing::take();
        // The same marks, folded into the split the build states before and
        // after the wait (carrick#1452). The two stages that ask the model are
        // the wait on the model; everything else in this loop, including the
        // packages read and the sidecar scoped above it, is this machine's.
        if let Some(totals) = &totals {
            crate::scan_timing::service_analysed(
                totals.local_secs() + packages_took.as_secs_f64() + sidecar_took.as_secs_f64(),
                totals.model_secs(),
            );
        }
        let phases = totals
            .as_ref()
            .map(crate::phase_timing::Totals::line)
            .unwrap_or_else(|| "not recorded".to_string());
        let requests = crate::agent_service::requests_between(
            &requests_before,
            &crate::agent_service::request_counts(),
        );
        info!(
            "Analyzed service {} in {:.1}s (packages {:.1}s, sidecar {:.1}s, analysis {:.1}s: \
             {}; requests {})",
            label,
            service_started.elapsed().as_secs_f64(),
            packages_took.as_secs_f64(),
            sidecar_took.as_secs_f64(),
            analysis_started.elapsed().as_secs_f64(),
            phases,
            requests,
        );

        let data = analysis.data;
        if data.bundled_types.is_some() {
            debug!(
                "Type resolution ({}): {} bundled types, {} manifest entries",
                data.service_name.as_deref().unwrap_or(&data.repo_name),
                data.bundled_types
                    .as_ref()
                    .map(|s| s.lines().count())
                    .unwrap_or(0),
                data.type_manifest.as_ref().map(|v| v.len()).unwrap_or(0)
            );
        }

        let owed = durability::OwedWork {
            deferred: analysis.deferred,
            losses: crate::scan_health::service_losses(service.service_name.as_deref()),
            retry_error: None,
        };
        Ok(ServiceRun {
            label: label.to_string(),
            data,
            owed,
            held_back: false,
        })
    }
}

/// The previous generation of `service` among `generations`, for the
/// incremental cache.
fn previous_generation(
    generations: &[CloudRepoData],
    repo_name: &str,
    service: &Config,
) -> Option<CloudRepoData> {
    generations
        .iter()
        .find(|r| r.repo_name == repo_name && r.service_name == service.service_name)
        .cloned()
}

/// Ship a dispatched run's prompts and say what came back (carrick#1229).
///
/// One thing can stop it: a service whose framework guidance has no id. The
/// cloud keys the whole message without one, so the block this bundle carries
/// once is re-keyed for every file, and the job is not sent.
async fn dispatch_analysis_job<T: CloudStorage + Sync>(
    storage: &T,
    collected: crate::analysis_channel::Collected,
    run: &crate::cloud_storage::RunContext,
    repo_path: &str,
) -> Result<crate::analysis_job::Dispatched, Box<dyn std::error::Error>> {
    // The name the cloud authorised the scan with, and this tree's own name
    // when it has no remote: never the working directory, which in a
    // multi-repo build is the workspace and is the same for every repo in it.
    let repo = run
        .repo_full_name
        .clone()
        .unwrap_or_else(|| get_repository_name(repo_path));
    if !collected.degraded.is_empty() {
        return Err(format!(
            "Carrick could not read the framework guidance for {}, so this scan cannot be handed \
             over. Run `carrick index` and it will analyse the repo here.",
            collected.degraded.join(", ")
        )
        .into());
    }
    let analyze_rows = collected.rows.len();
    let bundle = crate::analysis_job::JobBundle {
        header: crate::analysis_job::JobHeader {
            schema: crate::analysis_job::JOB_SCHEMA.to_string(),
            scan_id: crate::credentials::scan_id()
                .unwrap_or_default()
                .to_string(),
            repo: repo.clone(),
            commit: run.commit.clone(),
            scanner_version: env!("CARGO_PKG_VERSION").to_string(),
            cache_version: CACHE_VERSION,
            services: collected.services,
            guidance: collected.guidance,
            schemas: collected.schemas,
            counts: crate::analysis_job::JobCounts {
                analyze_file: analyze_rows,
                // Intents are generated by the machine that resumes: they are a
                // small fraction of a scan's calls once batched, and their
                // prompts embed answers that do not exist until the level below
                // them has been answered (carrick#1245).
                intent_functions: 0,
                intent_levels: 0,
            },
        },
        analyze: collected.rows,
    };
    let submission = storage
        .submit_analysis_job(&bundle)
        .await?
        .ok_or("Carrick Cloud did not take this analysis job")?;
    info!(
        "Carrick Cloud took job {} for {} file(s)",
        submission.job_id, submission.analyze_rows
    );
    Ok(crate::analysis_job::Dispatched {
        repo,
        commit: run.commit.clone(),
        job_id: submission.job_id,
        analyze_rows: submission.analyze_rows,
    })
}

/// The upload payloads for `services`, stripped and stamped with the state
/// of the tree they describe.
fn upload_payloads_for<T: CloudStorage>(
    storage: &T,
    repo_path: &str,
    services: &[&CloudRepoData],
    dirty: bool,
    unchanged_at_start: Option<&HashSet<String>>,
) -> Vec<CloudRepoData> {
    // Asked again now the files have been read, per commit the payloads name
    // (one, unless HEAD moved mid-scan), and kept only where both asks agree.
    let mut unchanged_by_commit: HashMap<String, Option<HashSet<String>>> = HashMap::new();
    services
        .iter()
        .map(|data| {
            let unchanged = unchanged_by_commit
                .entry(data.commit_hash.clone())
                .or_insert_with(|| {
                    let at_upload =
                        crate::git_state::unchanged_since(repo_path, &data.commit_hash).ok()?;
                    let at_start = unchanged_at_start?;
                    Some(at_upload.intersection(at_start).cloned().collect())
                });
            stamp_tree_state(
                strip_ast_nodes((*data).clone(), storage.stages_oversized_payloads()),
                dirty,
                unchanged.as_ref(),
            )
        })
        .collect()
}

/// Print each local service's boundary as the closing lines of the run.
///
/// Stdout, beside the report it belongs to. The eval projection mode returns
/// long before this, so the machine-readable output on stdout is never mixed
/// with it.
fn print_boundaries(boundaries: &[(String, crate::boundary::ServiceBoundary)]) {
    if boundaries.is_empty() {
        return;
    }
    crate::outln!("\nWhat this scan could not classify");
    for (service, boundary) in boundaries {
        for line in boundary.lines(service) {
            crate::outln!("{line}");
        }
    }
}

/// Each service's GraphQL schema lines for the report (carrick#1099): what a
/// `graphqlSchemas` declaration failed to declare, and the code-first hint for
/// a service that uses GraphQL, serves routes, and indexes no schema field.
///
/// `services` and `data` are the scan loop's, index for index. The GraphQL
/// libraries are the service's own declared dependencies plus what framework
/// detection reported, both passed through the one existing GraphQL library
/// filter, so the dependency half keeps the hint deterministic when detection
/// is cached or missing.
fn graphql_schema_notices(
    repo_path: &str,
    services: &[Config],
    data: &[CloudRepoData],
) -> crate::graphql::GraphqlNotices {
    let mut notices = crate::graphql::GraphqlNotices::default();
    for (service, data) in services.iter().zip(data) {
        let declared = crate::graphql::resolve_declared_schemas(
            Path::new(repo_path),
            &service.graphql_schemas,
        );
        let protocol_count = |protocol: crate::operation::Protocol| {
            data.endpoints
                .iter()
                .filter(|endpoint| endpoint.key.protocol() == protocol)
                .count()
        };
        let mut names: Vec<String> = Vec::new();
        if let Some(packages) = &data.packages {
            for manifest in &packages.package_jsons {
                names.extend(manifest.dependencies.keys().cloned());
                names.extend(manifest.dev_dependencies.keys().cloned());
                names.extend(manifest.peer_dependencies.keys().cloned());
            }
        }
        if let Some(detection) = &data.cached_detection {
            names.extend(detection.frameworks.iter().cloned());
            names.extend(detection.data_fetchers.iter().cloned());
        }
        let graphql_libraries = crate::analyzer::filter_graphql_libraries(&names);
        let label = service
            .service_name
            .as_deref()
            .or(data.service_name.as_deref())
            .unwrap_or(&data.repo_name);
        let service_notices =
            crate::graphql::service_notices(crate::graphql::GraphqlServiceFacts {
                service: label,
                declares_schemas: !service.graphql_schemas.is_empty(),
                declared: &declared,
                producers: protocol_count(crate::operation::Protocol::Graphql),
                serves_http: protocol_count(crate::operation::Protocol::Http) > 0,
                graphql_libraries: &graphql_libraries,
            });
        notices.warnings.extend(service_notices.warnings);
        notices.hints.extend(service_notices.hints);
    }
    notices
}

/// The report's GraphQL schema lines. A declaration that did nothing is a
/// warning, on the Actions step as well; the hint is a plain line.
fn print_graphql_notices(notices: &crate::graphql::GraphqlNotices) {
    if notices.warnings.is_empty() && notices.hints.is_empty() {
        return;
    }
    crate::outln!("\nGraphQL schemas");
    for warning in &notices.warnings {
        crate::outln!("  {warning}");
        logging::annotate(logging::Annotation::Warning, warning);
    }
    for hint in &notices.hints {
        crate::outln!("  {hint}");
    }
}

/// How much of this run's log file the upload reads, and from where: all of
/// it, up to its last 5 MB. A pathologically chatty run must not ship hundreds
/// of megabytes, and the tail is where a failure is.
fn log_tail_range(file_len: u64) -> (u64, usize) {
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    let start = file_len.saturating_sub(MAX_LOG_BYTES);
    (start, (file_len - start) as usize)
}

/// Best-effort upload of the current run's log tail to S3.
///
/// Reads this process's own log file, which no other process writes
/// (carrick#1133). It used to read the day's shared file from this process's
/// start offset, and every carrick process on the machine — other scans, a
/// test suite, the builds `index` and `refresh` run — wrote into that slice.
/// Capped at 5 MB in case a single run is unusually verbose.
///
/// Runs on both success and failure paths — failing runs are exactly the
/// ones whose logs we need. Errors here are non-fatal: a failed upload is
/// logged at warn but never propagated.
async fn upload_run_logs<T: CloudStorage>(storage: &T, repo_path: &str) {
    upload_run_log_from(storage, repo_path, logging::run_log_file()).await;
}

/// [`upload_run_logs`], reading `log_path` — the run's own file, or `None`
/// when this process has none.
pub(crate) async fn upload_run_log_from<T: CloudStorage>(
    storage: &T,
    repo_path: &str,
    log_path: Option<&Path>,
) {
    // Asked before the file is read: a backend with no cloud behind it (the
    // offline harness, the local join after a laptop scan) has nowhere to send
    // the log. CI and laptop scans both ship it (carrick#1063).
    //
    // Every return below says why nothing left the machine. They were silent,
    // and a run that uploaded nothing read exactly like a run that never got
    // here, which is how a scan on an older installed binary passed for a
    // defect in this function.
    if !storage.uploads_run_logs() {
        debug!(
            "Run log not uploaded: this run has nowhere to put it, either because the backend \
             ships no logs or because no scan of its own was ever opened"
        );
        return;
    }

    let Some(log_path) = log_path else {
        debug!("Run log not uploaded: this run has no log file of its own in ~/.carrick/logs/runs");
        return;
    };
    let mut file = match std::fs::File::open(log_path) {
        Ok(file) => file,
        Err(e) => {
            debug!("Run log not uploaded: could not open this run's debug log: {e}");
            return;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(e) => {
            debug!("Run log not uploaded: could not read the debug log's size: {e}");
            return;
        }
    };

    use std::io::{Read, Seek};

    let (start, expected) = log_tail_range(metadata.len());

    if let Err(e) = file.seek(std::io::SeekFrom::Start(start)) {
        debug!("Run log not uploaded: could not seek to offset {start} of this run's log: {e}");
        return;
    }

    let mut buf = Vec::with_capacity(expected);
    if let Err(e) = file.read_to_end(&mut buf) {
        debug!("Run log not uploaded: could not read this run's debug log: {e}");
        return;
    }

    // Note: `from_utf8_lossy` replaces invalid sequences with U+FFFD (3 bytes
    // in UTF-8), so the resulting `log_content.len()` may exceed the original
    // raw byte count when the file contains non-UTF-8 noise. Close enough for
    // a logged size hint.
    // What leaves the machine is not what is on it (carrick#1063, #1098): the
    // repo, the home directory, other machine paths and the account name are
    // rewritten, and any line naming a credential is dropped.
    // Done here rather than in the writer so the local copy stays complete for
    // whoever is debugging with it.
    let log_content =
        logging::Redaction::for_run(Some(repo_path)).log(&String::from_utf8_lossy(&buf));
    let repo_name = get_repository_name(repo_path);
    match storage.upload_logs(&repo_name, &log_content).await {
        Ok(()) => {
            debug!(
                bytes = log_content.len(),
                repo = %repo_name,
                "Uploaded run logs to S3"
            );
        }
        Err(e) if cloud_has_not_deployed_log_upload(&e.to_string()) => {
            // A cloud that does not serve `upload-logs` for this credential
            // yet. Nothing is wrong with the run, and a warning on the way out
            // of every scan would say there is.
            debug!("Carrick Cloud did not accept this run's log: {e}");
        }
        Err(e) => {
            warn!("Failed to upload logs: {}", e);
        }
    }
}

/// Whether a log-upload refusal is "this cloud has not deployed the action for
/// this credential", which is a 403 or a 404 and is not worth a warning.
///
/// Read out of the message because that is where the status is: the storage
/// errors are strings by the time they reach here, and `health_check` already
/// classifies its own refusals the same way. Deliberately narrow — only these
/// two statuses, and only as the standalone number the refusal formats.
fn cloud_has_not_deployed_log_upload(message: &str) -> bool {
    ["403", "404"].iter().any(|status| {
        message
            .split(|c: char| !c.is_ascii_digit())
            .any(|word| word == *status)
    })
}

/// Serialize CloudRepoData without AST nodes in ApiEndpointDetails
/// Generic function to merge serialized data from repo configs
fn merge_serialized_data<T>(
    all_repo_data: &[CloudRepoData],
    extractor: fn(&CloudRepoData) -> Option<&String>,
) -> Result<T, Box<dyn std::error::Error>>
where
    T: Default + serde::de::DeserializeOwned,
{
    // Special handling for Config to properly merge all configs
    if std::any::type_name::<T>() == std::any::type_name::<crate::config::Config>() {
        let mut temp_files = Vec::new();

        // Write each config to a temporary file
        for (i, repo_data) in all_repo_data.iter().enumerate() {
            if let Some(json_str) = extractor(repo_data) {
                let temp_path = std::env::temp_dir().join(format!("carrick_config_{}.json", i));
                if std::fs::write(&temp_path, json_str).is_err() {
                    continue;
                }
                temp_files.push(temp_path);
            }
        }

        // Use Config::new to properly merge all configs
        if !temp_files.is_empty() {
            let merged_config = crate::config::Config::new(temp_files.clone()).unwrap_or_default();

            // Clean up temp files
            for temp_file in temp_files {
                let _ = std::fs::remove_file(temp_file);
            }

            // This is a bit of a hack to return the merged config as T
            // Since we know T is Config when we get here
            let config_any = Box::new(merged_config) as Box<dyn std::any::Any>;
            if let Ok(config) = config_any.downcast::<crate::config::Config>() {
                let config_json = serde_json::to_string(&*config)?;
                return Ok(serde_json::from_str(&config_json)?);
            }
        }

        return Ok(T::default());
    }

    // For non-Config types, use the first found (original behavior)
    for repo_data in all_repo_data {
        if let Some(json_str) = extractor(repo_data)
            && let Ok(data) = serde_json::from_str::<T>(json_str)
        {
            return Ok(data);
        }
    }
    Ok(T::default())
}

/// Record what the tree looked like on the payload that describes it.
///
/// `dirty` qualifies `commit_hash`, which the blob already carries: without it
/// the blob says "this is the index at 4f2a1c9" about a tree that was not
/// 4f2a1c9, and every later reader of it as `previous_data` inherits the false
/// version.
///
/// `file_results` keeps only the answers about files whose bytes on disk are
/// the bytes the commit holds (`unchanged`, from
/// [`crate::git_state::unchanged_since`], asked before the files were read and
/// again after). The next run replays an answer only for a file whose content
/// still matches the previous scan's commit, so an answer about uncommitted
/// bytes would be replayed as soon as the edit was reverted, by every later
/// run, CI's included. Dropping exactly those answers stops that, and every
/// file the edit did not touch keeps its answer (carrick#1079): one untracked
/// file no longer costs a laptop the whole service's cache. A file git does
/// not track is never in `unchanged`, so its answer is never kept either.
///
/// When git could not answer (`None`), a dirty tree keeps no answers, because
/// nothing can say which ones describe the commit; a clean tree keeps them
/// all, because the next run cannot replay them without git anyway.
fn stamp_tree_state(
    mut payload: CloudRepoData,
    dirty: bool,
    unchanged: Option<&HashSet<String>>,
) -> CloudRepoData {
    payload.dirty = dirty.then_some(true);
    payload.file_results = match (payload.file_results.take(), unchanged) {
        (Some(results), Some(unchanged)) => {
            let kept: HashMap<_, _> = results
                .into_iter()
                .filter(|(path, _)| unchanged.contains(path))
                .collect();
            (!kept.is_empty()).then_some(kept)
        }
        (results, None) if !dirty => results,
        _ => None,
    };
    payload
}

/// When to ask the cloud whether a lost write landed, after the first ask.
///
/// The gateway cuts a response at 30 s while the handler runs on: on
/// 2026-09-14 the write the scanner never saw committed 25 s after the cut, so
/// a single check a few seconds later would have reported a successful upload
/// as lost and sent the user back for another of the day's three scans. Asking
/// four times over half a minute costs four scalar reads and covers that
/// window (carrick#1067).
const LANDED_CHECK_WAITS_SECONDS: [u64; 3] = [5, 10, 15];

/// One service whose index the run could not confirm.
#[derive(Debug, Clone, PartialEq)]
struct UnconfirmedUpload {
    service: String,
    reason: String,
}

/// When to ask whether a lost write landed, and how long to keep asking.
///
/// `None` is "don't ask": the cloud answered this one. A refusal that arrived
/// on its own — `409 partial_refused`, a rejected credential, a 413 — is a
/// decision about a write that never ran, and a landed-check would either
/// repeat the refusal or read the generation this run was replacing.
///
/// `Some(waits)` is a write whose outcome nobody saw. Empty waits ask once,
/// which is all there is to learn when nothing is still running behind the
/// failure; a handler that may still be committing is asked again after each
/// wait until it answers or the window closes.
fn landed_check_waits(error: &StorageError) -> Option<Vec<Duration>> {
    let write = error.uncertain_write()?;
    Some(if write.handler_may_still_run {
        LANDED_CHECK_WAITS_SECONDS
            .iter()
            .map(|seconds| Duration::from_secs(*seconds))
            .collect()
    } else {
        Vec::new()
    })
}

/// Did this service's index reach the cloud despite the error the upload
/// returned?
///
/// Only the storage backend can answer, and it is asked here rather than at
/// the call site so the waiting, the logging and the fail-closed rule live in
/// one place. A check that itself fails answers "no": the run reports a
/// service it could not confirm, which is true, instead of claiming one it
/// cannot see.
async fn upload_landed_anyway<T: CloudStorage>(
    storage: &T,
    payload: &CloudRepoData,
    service: &str,
    error: &StorageError,
    attempted_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(waits) = landed_check_waits(error) else {
        return false;
    };
    for (attempt, wait) in std::iter::once(Duration::ZERO).chain(waits).enumerate() {
        if !wait.is_zero() {
            debug!(
                "Waiting {}s before re-checking whether {service}'s index landed",
                wait.as_secs()
            );
            tokio::time::sleep(wait).await;
        }
        match storage.index_landed(payload, attempted_at).await {
            Ok(true) => {
                info!(
                    "{service} is indexed at this commit after {} check(s): the write landed and \
                     its response was lost, not the write",
                    attempt + 1
                );
                return true;
            }
            Ok(false) => continue,
            Err(check_error) => {
                warn!("Could not ask the cloud what it holds for {service}: {check_error}");
                return false;
            }
        }
    }
    false
}

/// Did this run send any of this service's files to the analyzer?
///
/// Read off the payload's own boundary, which every branch of the analysis
/// builds fresh from this scan's processing stats before the payload is
/// uploaded — so it is this run's count, not a number carried over from the
/// generation the index holds. A held-back service writes no payload at all,
/// so nothing here can be reading a served generation's boundary
/// (carrick#1306).
///
/// A payload with no boundary at all answers `false`: absence is not evidence
/// that analysis happened, and the alternative is forcing a re-index on every
/// upload.
fn payload_reached_the_analyzer(payload: &CloudRepoData) -> bool {
    payload
        .boundary
        .as_ref()
        .is_some_and(|boundary| boundary.files_attempted > 0)
}

/// Upload each already-prepared service payload to the cloud index, in order,
/// and report every service whose index this run could not confirm.
///
/// One service's upload never ends the run (carrick#1067). A write action that
/// fails after an attempt nobody saw the outcome of is answered by asking the
/// cloud what it holds — the index is frequently already current, because the
/// gateway cut a response while the handler committed — and a service that
/// really did not land is recorded and the next one is uploaded. The run exits
/// non-zero naming them, which is what the caller does with the returned list.
///
/// Uploads are keyed per (repo, service) and idempotent, so a re-run restores
/// consistency; until then the index is mixed-generation for this repo, and
/// the summary says so when some services landed and others did not.
async fn upload_service_payloads<T: CloudStorage>(
    storage: &T,
    payloads: &[CloudRepoData],
    forced: bool,
    // Whether the last write closes the scan. False when the run leaves a
    // service incomplete, which is closed by its fail marker instead.
    closes_run: bool,
    boundary: &upload_boundary::UploadBoundary,
) -> Vec<UnconfirmedUpload> {
    crate::scan_stage::enter(crate::scan_stage::Stage::Upload);
    // The third of the three waits a build states: this one is neither the
    // local read nor the model, and on a multi-service repo it is minutes
    // (carrick#1452).
    let upload_started = Instant::now();
    // Before the first write, because every write action reads it: a run that
    // sent even one file to the analyzer computed something the stored
    // generation does not hold, and the cloud's freshness guard — which sees
    // (commit, scanner version) and nothing else — would otherwise dedupe the
    // whole upload away at an unchanged commit (carrick#1306). Run-scoped, not
    // per service: one service's fresh answers reach the others through the
    // cross-repo join, so a repo where one service was analysed writes a
    // generation that differs everywhere.
    let analyzed = payloads.iter().any(payload_reached_the_analyzer);
    if analyzed {
        storage.note_analyzed_files();
    }
    // The diagnostic below asks whether the cloud discarded analysis this run
    // had done, so it has to mean the same thing the wire flag means.
    let forced = forced || analyzed;
    let sp = logging::spinner("Uploading results...");
    let mut outcomes: Vec<UploadOutcome> = Vec::with_capacity(payloads.len());
    let mut confirmed: Vec<&str> = Vec::new();
    let mut unconfirmed: Vec<UnconfirmedUpload> = Vec::new();
    for (i, payload) in payloads.iter().enumerate() {
        let service = payload
            .service_name
            .as_deref()
            .unwrap_or(&payload.repo_name);
        // Which service the run is on. A thirteen-service repo spends minutes
        // per upload, and without this the whole stage is one silent line.
        logging::progress(
            &sp,
            &format!("Uploading {service} ({}/{})", i + 1, payloads.len()),
        );
        // Only the last write action of the run releases the cloud's in-flight
        // scan slot: a multi-service repo sends N of them, and releasing on
        // the first would leave the rest of the run unprotected (§2.2).
        let final_in_run = closes_run && i + 1 == payloads.len();
        // When this write began, so a landed-check can tell the row it wrote
        // from the one it replaced — a forced run rewrites a row that already
        // carries this commit (carrick#1067).
        let attempted_at = chrono::Utc::now();
        // Defence in depth for machine paths (carrick#1204): whatever an
        // upstream pass left holding the checkout root or home goes up as a
        // placeholder. The payload in memory is not touched.
        let scrubbed = boundary.scrub(payload, service);
        let outgoing = scrubbed.as_ref().unwrap_or(payload);
        match storage.upload_repo_data(outgoing, final_in_run).await {
            Ok(outcome) => {
                outcomes.push(outcome);
                confirmed.push(service);
            }
            Err(e) => {
                warn!("Upload of {service} failed: {e}");
                if upload_landed_anyway(storage, payload, service, &e, attempted_at).await {
                    confirmed.push(service);
                } else {
                    unconfirmed.push(UnconfirmedUpload {
                        service: service.to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }
    }
    // What the run cost, from the write action that closed it. A laptop run
    // gets one; a CI upload never does, and neither does a cloud that has not
    // deployed the field — both of which say nothing rather than zero
    // (carrick#995). A run whose last response was lost has no figure either.
    if let Some(spend) = outcomes.iter().rev().find_map(|o| o.scan_spend.as_ref()) {
        crate::scan_spend::report(spend);
    }
    if !unconfirmed.is_empty() {
        let summary = unconfirmed_upload_summary(&confirmed, &unconfirmed);
        logging::finish_spinner_warn(&sp, &summary);
        warn!("{summary}");
        logging::annotate(logging::Annotation::Warning, &summary);
    } else if forced_reanalysis_was_discarded(&outcomes, forced) {
        // A scan that finished having done less than it was asked to: the
        // annotation puts it on the run summary, where someone who asked for
        // a full scan will look for the result of it.
        logging::finish_spinner_warn(&sp, FORCED_REANALYSIS_DISCARDED);
        warn!("{}", FORCED_REANALYSIS_DISCARDED);
        logging::annotate(logging::Annotation::Warning, FORCED_REANALYSIS_DISCARDED);
    } else {
        logging::finish_spinner(&sp, upload_finish_message(&outcomes));
    }
    crate::scan_timing::uploaded(upload_started.elapsed().as_secs_f64());
    unconfirmed
}

/// The run summary's sentence about the services that did not reach the index.
///
/// Only a PARTIAL upload leaves the index mixed-generation. When nothing
/// landed, nothing was replaced, and saying "mixed-generation" reads as damage
/// that needs a re-run — a paid one — over the top of an index that is exactly
/// as it was (carrick#1023 item 4).
fn unconfirmed_upload_summary(confirmed: &[&str], unconfirmed: &[UnconfirmedUpload]) -> String {
    let names: Vec<&str> = unconfirmed
        .iter()
        .map(|failed| failed.service.as_str())
        .collect();
    if confirmed.is_empty() {
        format!(
            "Nothing was uploaded: [{}] did not reach the index, which still holds what it held \
             before this scan.",
            names.join(", ")
        )
    } else {
        format!(
            "Uploaded: [{}]; not uploaded: [{}]. The index is mixed-generation for this repo \
             until a successful re-run.",
            confirmed.join(", "),
            names.join(", ")
        )
    }
}

/// What the run ends with when a service's index could not be confirmed: the
/// services, and the first thing each of them said.
///
/// The scan itself is finished and its report is already printed — the exit
/// code is what carries the failure, so this is the last line rather than an
/// abort partway through (carrick#1067).
fn unconfirmed_upload_error(unconfirmed: &[UnconfirmedUpload]) -> String {
    let detail: Vec<String> = unconfirmed
        .iter()
        .map(|failed| format!("{}: {}", failed.service, failed.reason))
        .collect();
    format!(
        "{} service(s) did not upload to the index ({}). The rest of the run completed; \
         re-run the scan to bring them up to this commit.",
        unconfirmed.len(),
        detail.join("; ")
    )
}

/// What the upload spinner says once every payload has landed.
///
/// "Uploaded" is a lie when the cloud short-circuited: it already held a row
/// for this commit hash and this scanner version, so nothing this run computed
/// was stored. Only claim that when EVERY service was skipped — a partial skip
/// in a multi-service repo did re-index something, and an empty slice never
/// uploaded anything to call current.
fn upload_finish_message(outcomes: &[UploadOutcome]) -> &'static str {
    if !outcomes.is_empty() && outcomes.iter().all(|o| o.already_current) {
        "Index already current for this commit and scanner version; nothing re-indexed"
    } else {
        "Uploaded results to Carrick Cloud"
    }
}

/// Said when a run that told the cloud to replace the stored generation had
/// its answers computed and then discarded by the ingest. Names the one cause
/// it can have, because the run itself did nothing wrong.
const FORCED_REANALYSIS_DISCARDED: &str = "This scan's fresh analysis was discarded: the cloud kept the stored index for this \
     commit. It is deployed without force_reindex support (carrick#885); the answers this \
     run computed were not stored.";

/// Did a forced run pay for a re-analysis the cloud then threw away?
///
/// A run that sent `force_reindex` and still got `already_current` back has
/// exactly one explanation: the deployed cloud predates the reader for that
/// field, since a cloud that has it never answers current to a forced write.
/// That is a deploy-order mistake, and it must show as a warning rather than
/// as the ordinary "already current" line — a forced run is one that believed
/// the stored answers were stale, whether because `--no-cache` said so or
/// because it sent files to the analyzer (carrick#885, carrick#1306).
fn forced_reanalysis_was_discarded(outcomes: &[UploadOutcome], forced: bool) -> bool {
    forced && !outcomes.is_empty() && outcomes.iter().all(|o| o.already_current)
}

/// Remove AST nodes from CloudRepoData for serialization, then run the payload
/// size guard. `staging_available` comes from the storage backend and decides
/// whether an oversized payload can keep its incremental caches — see
/// [`enforce_payload_size_limit`].
fn strip_ast_nodes(mut data: CloudRepoData, staging_available: bool) -> CloudRepoData {
    fn strip_endpoint_ast(endpoint: &mut ApiEndpointDetails) {
        endpoint.request_type = None;
        endpoint.response_type = None;
    }

    data.endpoints.iter_mut().for_each(strip_endpoint_ast);
    data.calls.iter_mut().for_each(strip_endpoint_ast);

    enforce_payload_size_limit(&mut data, staging_available);

    data
}

/// Payload size guard for the request body.
///
/// A payload over [`INLINE_PAYLOAD_LIMIT_BYTES`] is not sent in the request
/// body at all: `upload_repo_data` PUTs it to a presigned staging object and
/// the write action carries only a pointer. The staged object has no
/// request-size wall, so there is nothing to make room for and the incremental
/// caches are kept whole. `staging_available` is the backend's answer to
/// whether it does that (`CloudStorage::stages_oversized_payloads`); the
/// threshold below is the same one `upload_repo_data` measures against, so the
/// two decisions cannot drift.
///
/// Only when the backend does not stage does the request body have to fit, and
/// then the incremental caches (`file_results` being the multi-MB bulk) are
/// what gets dropped. Before carrick#536 the drop ran unconditionally, so every
/// large repo lost its caches and re-analyzed every file on the next scan even
/// though its payload had been staged.
///
/// Called twice per upload payload: inside [`strip_ast_nodes`] when the payload
/// is prepared, and again after [`crate::cloud_storage::attach_compat_verdicts`]
/// adds the per-pair type verdicts (#351) — anything appended after the first
/// pass could otherwise re-inflate the JSON past the cap. Degradation order is
/// deliberate: verdicts are tiny (a handful of short strings per cross-repo
/// edge) while `file_results` is the bulk, so the caches are always what gets
/// dropped; verdicts are kept.
fn enforce_payload_size_limit(data: &mut CloudRepoData, staging_available: bool) {
    // Sits above INLINE_PAYLOAD_LIMIT_BYTES, so any payload that reaches this
    // threshold is one `upload_repo_data` stages rather than inlines.
    const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024;
    let Ok(serialized) = serde_json::to_string(&data) else {
        return;
    };
    if serialized.len() <= MAX_PAYLOAD_BYTES {
        return;
    }

    if staging_available && serialized.len() > INLINE_PAYLOAD_LIMIT_BYTES {
        debug!(
            "Payload size {}KB is over the {}KB inline limit, so it will be staged to S3 \
             rather than sent in the request body; keeping the incremental caches",
            serialized.len() / 1024,
            INLINE_PAYLOAD_LIMIT_BYTES / 1024
        );
        return;
    }

    warn!(
        "Payload size {}KB exceeds {}KB limit and this backend does not stage oversized \
         payloads, so the file_results cache is dropped for this upload — the next scan \
         cannot run incrementally and will re-analyze every file",
        serialized.len() / 1024,
        MAX_PAYLOAD_BYTES / 1024
    );
    data.file_results = None;
    data.cached_detection = None;
    data.cached_guidance = None;

    // Re-check: if the body is still over the inline limit even without the
    // caches, say so up front — the upload will be rejected.
    if let Ok(reserialized) = serde_json::to_string(&data)
        && reserialized.len() > INLINE_PAYLOAD_LIMIT_BYTES
    {
        warn!(
            "Payload is still {}KB after dropping caches, over the {}KB the request body \
             can carry — the upload will be rejected.",
            reserialized.len() / 1024,
            INLINE_PAYLOAD_LIMIT_BYTES / 1024
        );
    }
}

/// The repo-relative paths whose content on disk is what `base_commit` holds,
/// so a previous scan's answer for them still describes them.
///
/// Compared against the working tree, not HEAD: a file edited and not yet
/// committed is changed here whatever `git diff <base> HEAD` says, and a file
/// git does not track is never unchanged (carrick#1079). `None` is "git could
/// not answer", and the caller falls back to a full analysis.
fn reusable_paths(repo_path: &str, base_commit: &str) -> Option<HashSet<String>> {
    let reason = match crate::git_state::unchanged_since(repo_path, base_commit) {
        Ok(paths) => return Some(paths),
        Err(reason) => reason,
    };
    // Surface this at warn level with the cause: a shallow clone
    // (actions/checkout defaults to fetch-depth: 1) silently forces a full
    // re-analysis — including its full LLM cost — on every run.
    let is_shallow = std::process::Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
    if is_shallow {
        warn!(
            "Incremental mode unavailable: this is a shallow clone, so the previous \
             scan's commit isn't reachable for diffing. Set `fetch-depth: 0` on \
             actions/checkout to avoid re-analyzing every file on each run."
        );
    } else {
        warn!(
            "Incremental mode unavailable: git diff against {} failed ({}). \
             Falling back to full analysis.",
            base_commit, reason
        );
    }
    None
}

/// Hash file content for cache invalidation (package.json).
fn hash_file_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Hash discovered manifests and Deno resolution inputs (sorted by relative path)
/// for the detection/guidance/extraction-config cache gate. The artifacts
/// behind the gate are generated from the MERGED dependency set, so the gate
/// must cover workspace manifests too — hashing only the root package.json
/// would let a dependency added in `packages/api/package.json` reuse a stale
/// extraction config (and stale detection) indefinitely.
fn hash_workspace_package_jsons(
    packages: &Packages,
    repo_path: &str,
) -> Result<String, std::io::Error> {
    let repo_root = Path::new(repo_path);
    let inputs = crate::deno_support::resolution_inputs(repo_root, &packages.source_paths)?;
    let mut keyed: Vec<(String, &PathBuf)> = inputs
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(repo_root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            (relative, path)
        })
        .collect();
    keyed.sort();

    let mut combined = String::new();
    for (relative, path) in keyed {
        combined.push_str(&relative);
        combined.push('\0');
        match std::fs::read_to_string(path) {
            Ok(content) => {
                combined.push_str(&content);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                combined.push_str("missing")
            }
            Err(error) => return Err(error),
        }
        combined.push('\0');
    }
    Ok(hash_file_content(&combined))
}

/// Normalize file_results keys to be relative to repo root.
/// This ensures cache key consistency between runs.
fn normalize_file_results_keys(
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
) -> HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult> {
    let repo_prefix = if repo_path.ends_with('/') {
        repo_path.to_string()
    } else {
        format!("{}/", repo_path)
    };

    file_results
        .iter()
        .map(|(key, value)| {
            let normalized_key = key
                .strip_prefix(&repo_prefix)
                .or_else(|| key.strip_prefix("./"))
                .unwrap_or(key)
                .to_string();
            (normalized_key, value.clone())
        })
        .collect()
}

/// Which of this scan's files already hold a model answer, so the analyzer
/// need not be asked about them again.
///
/// Every discovered file is analysed either way: the deterministic layer runs
/// over all of them on every scan (see `CACHE_VERSION`), and this decides only
/// which ones cost a model call. A file is reusable when the previous scan
/// recorded an answer for it and its content on disk is still what that scan's
/// commit holds (`unchanged`, from [`reusable_paths`]). A file the previous
/// scan never recorded — new, one phase 1 skipped,
/// or one whose call failed — has no entry and goes back through phase 1, which
/// dispatches it only if it raises a candidate this time. That is the whole
/// mechanism by which a scanner improvement reaches an indexed repo: the skip
/// is re-decided every scan rather than frozen into the cache.
///
/// `discovered` yields each file twice-keyed: first as the ORCHESTRATOR keys it
/// (the path as it appears in the discovered set), then as the CACHE keys it
/// (repo-relative). The returned map carries the orchestrator's key, so the two
/// conventions meet here rather than inside the orchestrator.
fn reusable_model_answers(
    discovered: impl Iterator<Item = (String, String)>,
    previous: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    unchanged: &HashSet<String>,
) -> HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult> {
    discovered
        .filter(|(_, relative)| unchanged.contains(relative))
        .filter_map(|(key, relative)| Some((key, previous.get(&relative)?.clone())))
        .collect()
}

/// Where a service's previous generation comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviousGeneration {
    /// The blob an earlier scan stored (or none at all).
    Stored,
    /// This run's own blob, re-entered to retry the work it still owes. What
    /// it holds was asked for minutes ago, so nothing in it is asked again
    /// for its own sake (carrick#1564: the library-semantics re-ask).
    ThisRun,
}

/// Incremental analysis: reuse cached per-file LLM results for unchanged files.
#[allow(clippy::too_many_arguments)]
async fn analyze_current_repo_incremental(
    repo_path: &str,
    service: &Config,
    packages: &Packages,
    sidecar: Option<&TypeSidecar>,
    previous_data: Option<&CloudRepoData>,
    generation: PreviousGeneration,
    workspace: &mut crate::external_call_candidates::WorkspaceScan,
    run_intents: &RunIntentMemo,
    graphql_schemas: &crate::graphql::SchemaCatalogue,
) -> Result<ServiceAnalysis, Box<dyn std::error::Error>> {
    let start = Instant::now();

    // Canonicalize repo_path for consistent path normalization between runs
    let canonical = std::fs::canonicalize(repo_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| repo_path.to_string());
    let repo_path = canonical.as_str();

    let config = service;

    // Discover files and symbols (fast SWC pass, always full), scoped to the service
    let cm: Lrc<SourceMap> = Default::default();
    let FileDiscovery {
        files,
        import_facts: all_import_facts,
        function_definitions,
        repo_name,
        request_inputs,
    } = discover_files_and_symbols(repo_path, config, cm.clone())?;
    crate::phase_timing::mark(crate::phase_timing::Phase::Discover);

    // 3. Check if we can use incremental mode. The cache version is the whole
    // test. A generation that stored no file answers (none of its files was
    // asked about, or every answered file was edited when it was written)
    // holds an empty answer cache, not no generation: its detection and
    // guidance replay under the same manifest gate as any other service's.
    // Reading it as no generation asked detection and every guidance section
    // again on every scan (carrick#1746).
    let can_use_incremental = previous_data.filter(|prev| {
        let version_matches = prev.cache_version == Some(CACHE_VERSION);
        if !version_matches {
            debug!(
                "Cache version mismatch (expected {}, got {:?}), running full analysis",
                CACHE_VERSION, prev.cache_version
            );
        }
        version_matches
    });

    if let Some(prev) = can_use_incremental {
        let prev_commit = &prev.commit_hash;
        debug!(
            "Found previous analysis (commit {})",
            &prev_commit[..std::cmp::min(7, prev_commit.len())]
        );

        // Which files still hold the content the previous scan's commit did.
        if let Some(unchanged) = reusable_paths(repo_path, prev_commit) {
            // Function intents need only discovery's definitions and the
            // previous scan's hashes, so they start now and run beside
            // detection, file analysis and the graph (carrick#1065). Started
            // only once this branch is committed to: the fallback below runs
            // its own discovery and would otherwise pay for intents twice.
            //
            // Caching is content-addressed: a `content_hash -> intent` map from
            // the previous scan lets the generator reuse an intent whenever a
            // function's body and its callees' intents are unchanged, without
            // re-calling /generate-intent. A caller in an unchanged file is
            // still refreshed when one of its callees changed (the caller's
            // hash includes its callee intents). Incremental scans populate
            // FunctionDefinition.intent exactly as full ones do (issue #110).
            let intents = IntentsInFlight::start(
                AgentService::new(),
                function_definitions,
                PreviousIntents::from_definitions(&prev.function_definitions),
                run_intents.clone(),
            );

            let no_answers = HashMap::new();
            let prev_file_results = prev.file_results.as_ref().unwrap_or(&no_answers);
            let repo_prefix = format!("{}/", repo_path);

            // Helper to normalize a file path to repo-relative
            let normalize_path = |f: &PathBuf| -> String {
                let s = f.to_string_lossy();
                if let Some(stripped) = s.strip_prefix(&repo_prefix) {
                    stripped.to_string()
                } else if let Some(stripped) = s.strip_prefix("./") {
                    stripped.to_string()
                } else {
                    s.to_string()
                }
            };

            let cached_model_results = reusable_model_answers(
                files
                    .iter()
                    .map(|f| (f.to_string_lossy().to_string(), normalize_path(f))),
                prev_file_results,
                &unchanged,
            );

            let total_files = files.len();
            let reused_count = cached_model_results.len();
            // Files with no cached answer, which is not the same as files the
            // model will be asked about: most of them are the ones phase 1
            // skips at zero cost every scan, and only those raising a candidate
            // are dispatched (see `reusable_model_answers`).
            let uncached_count = total_files - reused_count;

            debug!(
                "{} of {} file(s) have no cached answer, replaying {} cached answer(s)",
                uncached_count, total_files, reused_count
            );

            // Check if any package.json changed → need fresh framework
            // detection/guidance/extraction config. Covers workspace
            // manifests, not just the repo root (raw file content, not the
            // serialized struct, for deterministic comparison).
            let current_pkg_hash = hash_workspace_package_jsons(packages, repo_path)?;

            let pkg_changed = prev.package_json_hash.as_deref() != Some(&current_pkg_hash);

            // Get framework detection, guidance, and extraction config
            // (cached or fresh — all three share the package_json_hash gate).
            // A fresh ask that fails defers this service's model analysis
            // instead of ending the run (see `model_setup`).
            let mut setup = if crate::local_mode::no_model() {
                if !pkg_changed {
                    ModelSetup::ready(
                        prev.cached_detection.clone().unwrap_or_default(),
                        prev.cached_guidance
                            .clone()
                            .unwrap_or_else(crate::local_mode::offline_guidance),
                        prev.cached_extraction_config.clone(),
                    )
                } else {
                    ModelSetup::ready(
                        DetectionResult::default(),
                        crate::local_mode::offline_guidance(),
                        None,
                    )
                }
            } else if !pkg_changed {
                if let (Some(det), Some(guid)) = (
                    &prev.cached_detection,
                    prev.cached_guidance
                        .as_ref()
                        .filter(|g| guidance_is_keyed(g)),
                ) {
                    debug!("Reusing cached framework detection and guidance");
                    let det = reask_client_semantics(
                        det,
                        generation,
                        &service_scan_root(repo_path, config),
                        Path::new(repo_path),
                        || async {
                            FrameworkDetector::new(reask_agent())
                                .detect_frameworks_and_libraries(packages, &all_import_facts)
                                .await
                        },
                        say_schedule,
                    )
                    .await;
                    // A missing cached config (older cache entry, or an
                    // earlier failed generation) is regenerated on its own.
                    let extraction = match &prev.cached_extraction_config {
                        Some(config) => Some(config.clone()),
                        None => {
                            let agent = FrameworkGuidanceAgent::new(AgentService::new());
                            generate_extraction_config(&agent, &det, packages).await
                        }
                    };
                    ModelSetup::ready(det, guid.clone(), extraction)
                } else {
                    // Something is missing: a first scan's cache entry, the
                    // service a previous scan deferred, or a blob whose
                    // guidance predates the guidance id (carrick#1224). A
                    // detection that landed without usable guidance is kept,
                    // and only the guidance is asked again (carrick#1126).
                    if prev.cached_guidance.is_some() {
                        debug!(
                            "Cached guidance carries no id (written before the id existed); asking for guidance again so the analysis cache keys it by identity, not by its text"
                        );
                    }
                    model_setup(
                        packages,
                        &all_import_facts,
                        SettledDetection::kept_by(prev, &current_pkg_hash),
                    )
                    .await
                }
            } else {
                debug!("package.json changed, re-running framework detection");
                model_setup(packages, &all_import_facts, None).await
            };
            // The in-scan schedule starts now, so it runs beside everything
            // below (carrick#1564).
            let settling = start_semantics_schedule(
                &setup.detection,
                semantics_schedule_applies(generation),
                packages,
                &all_import_facts,
                &service_scan_root(repo_path, config),
                Path::new(repo_path),
            );

            // What this scan actually dispatches is decided per file inside the
            // orchestrator — a file with no cached answer still reaches the
            // model only if phase 1 raises a candidate for it — and it logs the
            // dispatched and replayed counts once it knows them.
            let agent_service = AgentService::new();
            let (summaries_sender, summaries) = tokio::sync::oneshot::channel();
            let file_orchestrator = FileOrchestrator::new(agent_service.clone())
                .deferring_model(setup.deferred.is_some())
                .with_request_summaries(crate::agents::file_orchestrator::SummarySource::later(
                    summaries,
                ));

            // Stage B2: GraphQL producer field-list from the service's SDL,
            // derived deterministically so the file-analyzer can emit
            // `graphql_operations` linking resolvers to schema fields. Scanned
            // over the full service `files` (not just `files_to_analyze`) so the
            // producer list is complete; empty for non-GraphQL services.
            let graphql_producer_hints = crate::graphql::GraphqlProducerHints::collect(
                service_graphql_roots(repo_path, service),
                &crate::graphql::resolve_declared_schemas(
                    Path::new(repo_path),
                    &service.graphql_schemas,
                )
                .files,
                &files,
            );
            // #268: the consumer mirror — document consumers with no
            // deterministic call-site anchor, so the file-analyzer can locate
            // their co-located result type. Same scan-root/files inputs as the
            // producer hints; empty for services with no unanchored consumers.
            let graphql_consumer_hints = crate::graphql::GraphqlConsumerHints::collect(
                service_graphql_roots(repo_path, service),
                &files,
                repo_path,
            );

            let normalizer = UrlNormalizer::new(config);
            let service_root = service_scan_root(repo_path, config);
            crate::phase_timing::mark(crate::phase_timing::Phase::Cache);
            // One index for this service, built before the analysis and shared
            // with the type requests below (carrick#1416): the join passes that
            // run after the model resolve a call's specifier with the same
            // resolver the sidecar's requests do.
            let service_modules = service_module_index(repo_path, config);
            // Analysis runs over EVERY discovered file, cached or not: the
            // deterministic layer is re-derived from the AST on every scan and
            // only the model's answer is replayed. That is what lets a resolver
            // fix reach this repo without a model call — and it is why there is
            // no merge with the previous scan's rows below: this run states
            // them all, and a deleted file simply has no row.
            enter_stage(crate::scan_stage::Stage::FileAnalysis)?;
            // The summaries are composed once the schedule started above has
            // settled; the analysis reads them only once the model has been
            // asked (carrick#1564).
            let declared_dependencies = packages.declared_dependency_names();
            let semantics_root = service_scan_root(repo_path, config);
            let library_ask = LibraryAsk::start(
                &request_inputs,
                sidecar,
                &semantics_root,
                Path::new(repo_path),
            );
            let (analysis, settled, library_answer) = tokio::join!(
                file_orchestrator.analyze_files(
                    &files,
                    &cached_model_results,
                    &setup.guidance,
                    &setup.detection,
                    &service_root,
                    Path::new(repo_path),
                    &declared_dependencies,
                    &graphql_producer_hints,
                    &graphql_consumer_hints,
                    &normalizer,
                    &service_modules,
                    sidecar,
                ),
                compose_summaries(
                    &request_inputs,
                    settling,
                    sidecar,
                    &semantics_root,
                    summaries_sender,
                ),
                first_library_answer(library_ask.as_ref()),
            );
            let analysis = analysis?;
            setup.detection.client_semantics = settled;
            crate::phase_timing::mark(crate::phase_timing::Phase::Model);

            let merged_results = normalize_file_results_keys(&analysis.file_results, repo_path);
            let raw_model_results =
                normalize_file_results_keys(&analysis.raw_model_results, repo_path);

            // Rebuild mount graph from full merged results
            let graph_orchestrator = FileOrchestrator::new(agent_service.clone());
            // `merged_results` keys were normalized to repo-relative paths
            // above, so provenance classification resolves against "" here.
            let mut mount_graph = graph_orchestrator.build_mount_graph(
                &merged_results,
                &normalizer,
                std::path::Path::new(""),
                // Keys are repo-relative here, so the import/mount resolution
                // needs the repo root to reach the modules on disk.
                std::path::Path::new(repo_path),
            );
            crate::phase_timing::mark(crate::phase_timing::Phase::Graph);

            // Declared operations (carrick#831), applied on the incremental
            // path exactly as on the full one. The blocks live in the config,
            // not in the analysis cache, so an edited `carrick.json` takes
            // effect on the next scan without a cache bump.
            let declared = crate::dispatch::apply_declared_operations(&mut mount_graph, service);
            if declared > 0 {
                debug!("Declared operations materialised from carrick.json: {declared}");
            }

            // Deterministic protocol scans run BEFORE the graph is projected:
            // the GraphQL consumer file set folds transport data calls out of
            // the graph (#307) so every downstream surface (cloud projection,
            // type manifest, type requests) sees the same call set.
            let (mut protocol_extractions, document_sites) = scan_protocol_extractions(
                repo_path,
                service,
                &files,
                &merged_results,
                &setup.detection.socket_clients,
            );
            settle_graphql_documents(
                &mut protocol_extractions.graphql,
                document_sites,
                &merged_results,
                repo_path,
                &service_modules,
                &mut mount_graph,
                service,
                graphql_schemas,
            );
            protocol_extractions.library = library_rows(
                library_ask.as_ref(),
                library_answer,
                sidecar,
                &semantics_root,
            )
            .await;
            let withdrawn = withdraw_model_routes_at_definitions(
                &mut mount_graph,
                &merged_results,
                &protocol_extractions.library,
                repo_path,
            );
            if withdrawn > 0 {
                debug!("Model routes withdrawn at library definitions: {withdrawn}");
            }
            crate::phase_timing::mark(crate::phase_timing::Phase::Protocols);

            // Collect the intents started after discovery (body_source is
            // stripped from every definition by now). `Intents` times only the
            // wait left at this point; the rest overlapped the stages above.
            enter_stage(crate::scan_stage::Stage::Intents)?;
            let mut function_definitions = intents.finish().await;
            crate::phase_timing::mark(crate::phase_timing::Phase::Intents);

            enter_stage(crate::scan_stage::Stage::Signatures)?;
            // Compose function signatures, inferring unannotated slots via sidecar.
            populate_function_signatures(
                signature_sidecar(sidecar),
                &mut function_definitions,
                repo_path,
            );
            crate::phase_timing::mark(crate::phase_timing::Phase::Signatures);
            enter_stage(crate::scan_stage::Stage::BlobBuild)?;

            let elapsed = start.elapsed();
            debug!(
                "Incremental analysis complete in {:.1}s",
                elapsed.as_secs_f64()
            );

            // Build CloudRepoData with merged results
            let mut cloud_data = build_cloud_data_from_mount_graph(
                &repo_name,
                repo_path,
                &mount_graph,
                config,
                packages,
                function_definitions,
            );
            let in_process_pubsub = classify_in_process_pubsub(
                repo_path,
                &merged_results,
                &cloud_data,
                &service_modules,
                &setup.detection,
                &protocol_extractions.event_bus,
            );
            append_deterministic_protocol_operations(
                &mut cloud_data,
                &protocol_extractions,
                &merged_results,
                &in_process_pubsub,
                repo_path,
                service,
            );
            attach_external_call_candidates(&mut cloud_data, repo_path, &files, config, workspace);
            attach_sdk_surface(&mut cloud_data, repo_path, config);
            crate::phase_timing::mark(crate::phase_timing::Phase::Surface);

            // Populate cache fields. The cache holds the MODEL's answers, not
            // the joined rows: see CACHE_VERSION. Nothing is stripped from them
            // — `candidate_id` is the key the next scan's join reaches the
            // candidate's span by (`span:LO-HI`, computed against a per-file
            // SourceMap, so it is stable while the file is), and the client
            // name and payload locators are the model's contribution to the
            // row, which a replay has to carry as a cold scan would.
            // Handlers that switch on a request field (carrick#831), read
            // off the same model answers the cache holds. A fact about the
            // handler, kept beside the operations because a routeless one has
            // no operation row to carry it.
            cloud_data.dispatch_tables =
                crate::dispatch::collect_dispatch_tables(&raw_model_results);
            // ...and onto the handler's own function row, joined by name and
            // declaration line, so a reader of a function row finds the fact
            // without knowing the array exists.
            if let Some(tables) = cloud_data.dispatch_tables.as_ref() {
                let stamped = crate::dispatch::stamp_dispatch_tables_on_functions(
                    &mut cloud_data.function_definitions,
                    tables,
                );
                debug!("Dispatch tables stamped onto function rows: {stamped}");
            }
            cloud_data.file_results = Some(raw_model_results);
            // A deferred service caches no detection, guidance or extraction
            // config: their absence is what makes the next scan ask for them
            // again, and every file it could not send keeps no answer, so that
            // scan dispatches exactly those.
            setup.stamp_cache(&mut cloud_data);
            cloud_data.package_json_hash = Some(current_pkg_hash);
            cloud_data.cache_version = Some(CACHE_VERSION);

            // Every type request below names the file a type was imported
            // from, and the specifier it was imported by is the repo's to
            // resolve: one index reads the config that governs each file
            // (carrick#1416). It is the one built before the analysis above,
            // so the join passes and the request builders resolve alike.
            //
            // Build type manifest
            let mut manifest_entries = build_type_manifest_entries(&mount_graph, config, repo_path);
            drop_call_through_entries(&mut manifest_entries, &merged_results);
            stamp_manifest_anchor_symbols(
                &mut manifest_entries,
                &merged_results,
                repo_path,
                &service_modules,
            );
            let library_sites = LibrarySiteIndex::of(
                &stated_library_rows(&protocol_extractions.library, repo_path, service),
                repo_path,
            );
            append_protocol_manifest_entries(
                &mut manifest_entries,
                &protocol_extractions,
                &library_sites,
            );
            append_pubsub_manifest_entries(
                &mut manifest_entries,
                &merged_results,
                &protocol_extractions.sockets,
                &in_process_pubsub,
                &library_sites,
                repo_path,
            );
            if !manifest_entries.is_empty() {
                cloud_data.type_manifest = Some(manifest_entries);
            }

            // Socket payload anchors and GraphQL consumer result-type anchors
            // both resolve through the same sidecar bundle path as HTTP explicit
            // symbols (#245/#248). Concatenate both into the extra-explicit slice.
            let mut protocol_requests = file_orchestrator.collect_socket_type_requests(
                &library_sites.typed_sockets(&protocol_extractions.sockets),
                repo_path,
                &service_modules,
            );
            protocol_requests.extend(file_orchestrator.collect_graphql_type_requests(
                &protocol_extractions.graphql,
                repo_path,
                &service_modules,
            ));
            // Pub/sub ops are LLM-sourced in `merged_results`, not in the
            // deterministic `protocol_extractions`, so their payload anchors
            // bundle through the same path (#corpus-2 resolution dim). A row
            // withdrawn as in-process has no operation to type (carrick#1513).
            let pubsub_results = in_process_pubsub.pubsub_rows_kept(&merged_results);
            protocol_requests.extend(file_orchestrator.collect_pubsub_type_requests(
                &pubsub_results,
                repo_path,
                &service_modules,
            ));

            // GraphQL producers take the infer path, not the bundle path: their
            // response contract is the resolver's expanded RETURN type, so they
            // become `FunctionReturn` infer requests (Stage B1).
            let mut protocol_infer = file_orchestrator.collect_graphql_producer_infer_requests(
                &protocol_extractions.graphql,
                repo_path,
                &service_modules,
            );
            // Pub/sub payloads with no named symbol (wrapper patterns:
            // topic-map emitters, schema-catalog workers, generic channel
            // handles) resolve via the LLM-located payload expression through
            // the same infer path.
            protocol_infer.extend(
                file_orchestrator.collect_pubsub_infer_requests(&pubsub_results, repo_path),
            );
            // A GraphQL consumer row whose executed document declares the
            // field's result type reads that type where it is declared
            // (carrick#1761).
            protocol_infer.extend(
                file_orchestrator
                    .collect_graphql_consumer_infer_requests(&protocol_extractions.graphql),
            );

            crate::phase_timing::mark(crate::phase_timing::Phase::Manifest);

            // Type resolution via sidecar (+ v2 capture stub for this service)
            let stub_dir = resolve_types_if_available(
                sidecar,
                &file_orchestrator,
                &merged_results,
                repo_path,
                setup.extraction_config.as_ref(),
                &mount_graph,
                config,
                &service_modules,
                &protocol_requests,
                &protocol_infer,
                &mut cloud_data,
            );
            crate::phase_timing::mark(crate::phase_timing::Phase::Types);

            if let Some(bundled_types) = cloud_data.bundled_types.take() {
                let updated =
                    append_missing_aliases(bundled_types, cloud_data.type_manifest.as_ref());
                cloud_data.bundled_types = Some(updated);
            }

            // Resolve per-endpoint definitions from the capture stub tree
            if let (Some(sidecar), Some(stub_dir)) = (sidecar, stub_dir.as_deref()) {
                resolve_per_endpoint_definitions(sidecar, &mut cloud_data, stub_dir);
            }
            crate::phase_timing::mark(crate::phase_timing::Phase::Definitions);
            if let Some(stub_dir) = stub_dir {
                let _ = std::fs::remove_dir_all(&stub_dir);
            }

            // Last step on this branch: everything above resolves against the
            // absolute tree on disk, everything after this reads the payload as
            // index data. `repo_path` is the canonicalized root the whole
            // function ran against, so the strip is exact.
            relativize_cloud_paths(
                &mut cloud_data,
                repo_path,
                &served_paths::PathScrub::for_scan(repo_path),
            );
            let placed = crate::handler_span::attach_handler_spans(&mut cloud_data);
            debug!("Handler spans placed on endpoint rows: {placed}");

            // Same last step as the full branch: the boundary is read off the
            // finished payload once its paths are repo-relative (carrick#705).
            cloud_data.boundary = Some(crate::boundary::ServiceBoundary::collect(
                &cloud_data,
                &analysis.stats,
                &merged_results,
                repo_path,
            ));
            if crate::local_mode::no_model() {
                cloud_data
                    .boundary
                    .as_mut()
                    .unwrap()
                    .candidates_withheld_changed_files = Some(
                    prev_file_results
                        .keys()
                        .filter(|file| !unchanged.contains(*file))
                        .count(),
                );
            }
            // Everything between the marks above: manifest assembly, the
            // deterministic attachments, relativisation. Named so the printed
            // phases add up to `analysis` rather than falling short of it.
            crate::phase_timing::mark(crate::phase_timing::Phase::Other);

            return Ok(ServiceAnalysis {
                data: cloud_data,
                deferred: setup.deferred,
            });
        } else {
            debug!("git could not compare the tree, falling back to full analysis");
        }
    }

    // Fallback: full analysis (analyze_current_repo now populates cache fields)
    debug!("Running full analysis...");
    // The intent content-hash cache is keyed purely on content
    // (INTENT_CACHE_VERSION + body + callee intents), so it stays valid even
    // when the ANALYSIS cache is unusable (cache_version bump, missing
    // file_results, shallow clone). Seed the full scan with the previous
    // scan's intents so a full re-analysis re-pays /generate-intent only for
    // functions whose content actually changed.
    let prev_intents = previous_data
        .map(|prev| PreviousIntents::from_definitions(&prev.function_definitions))
        .unwrap_or_default();
    // A service whose previous pass deferred its guidance carries no file
    // answers, so it always lands here; its detection is kept all the same
    // (carrick#1126).
    let settled = match previous_data {
        Some(prev) => {
            SettledDetection::kept_by(prev, &hash_workspace_package_jsons(packages, repo_path)?)
        }
        None => None,
    };
    // Discovery above already parsed every file, resolved the call edges and
    // walked the manifests; the full analysis reads the same result rather
    // than running all of it a second time (carrick#1108).
    let analysis = analyze_current_repo(
        repo_path,
        config,
        packages,
        sidecar,
        Discovered {
            cm,
            files,
            import_facts: all_import_facts,
            function_definitions,
            repo_name,
            request_inputs,
        },
        prev_intents,
        settled,
        workspace,
        run_intents,
        graphql_schemas,
    )
    .await?;

    let elapsed = start.elapsed();
    debug!("Full analysis complete in {:.1}s", elapsed.as_secs_f64());

    Ok(analysis)
}

/// Ask `/framework-detect` again for a cached detection that has no library
/// semantics yet, or has one still `pending` (carrick#1564), when one of its
/// data fetchers is installed and could verify an answer. This replaces a
/// `CACHE_VERSION` bump, which would re-analyse every repo cold.
///
/// Returns the cached detection with the answer's semantics, whatever lists
/// the answer names (carrick#1606). The lists and notes, and the guidance and
/// extraction config keyed on them, move only when a manifest moves (the
/// `package_json_hash` gate); an ask for semantics is not that.
///
/// Best-effort: `ask` is one HTTP attempt ([`reask_agent`]) bounded by
/// [`crate::client_semantics::PENDING_REASK_TIMEOUT`], announced to the user
/// through `notice` first, and made at most once a run, never when the
/// cached detection is this run's own (a retry of owed work). A failure, or
/// no answer in time, keeps the cached detection as it is and never defers
/// the service.
///
/// What `notice` returns is held until the ask has returned, failed or run
/// out of time, and then dropped: [`say_schedule`]'s guard ends the notice
/// there (carrick#1674).
async fn reask_client_semantics<Ask, AskFut, Shown>(
    cached: &DetectionResult,
    generation: PreviousGeneration,
    service_root: &Path,
    repo_root: &Path,
    ask: Ask,
    notice: impl FnOnce(crate::client_semantics::ScheduleNotice) -> Shown,
) -> DetectionResult
where
    Ask: FnOnce() -> AskFut,
    AskFut: std::future::Future<Output = Result<DetectionResult, Box<dyn std::error::Error>>>,
{
    if crate::local_mode::no_model()
        || generation == PreviousGeneration::ThisRun
        || !crate::client_semantics::wants_reask(
            cached.client_semantics.as_deref(),
            &cached.data_fetchers,
            service_root,
            repo_root,
        )
    {
        return cached.clone();
    }
    debug!("Asking framework detection again for this service's library semantics");
    let shown = notice(crate::client_semantics::reask_notice(
        cached.client_semantics.as_deref(),
        &cached.data_fetchers,
        service_root,
        repo_root,
    ));
    let answered =
        tokio::time::timeout(crate::client_semantics::PENDING_REASK_TIMEOUT, ask()).await;
    // The wait is over whichever way it went.
    drop(shown);
    match answered {
        Ok(Ok(fresh)) => {
            if !fresh.same_lists(cached) {
                debug!(
                    "Asking framework detection again for library semantics answered other packages than the cached detection; keeping the cached lists and taking only the semantics"
                );
            }
            DetectionResult {
                client_semantics: fresh.client_semantics,
                ..cached.clone()
            }
        }
        // Nothing for the user to act on: the next scan asks again.
        Ok(Err(error)) => {
            debug!(
                "Asking framework detection again for library semantics failed ({error}); keeping the cached detection"
            );
            cached.clone()
        }
        Err(_) => {
            debug!(
                "Asking framework detection again for library semantics ran out of time; keeping the cached detection"
            );
            cached.clone()
        }
    }
}

/// The client the library-semantics re-ask is sent with: one HTTP attempt,
/// no retry, because a failure costs nothing the next scan does not fix.
pub(crate) fn reask_agent() -> AgentService {
    AgentService::new().with_retry_policy(crate::agent_service::RetryPolicy::ONCE)
}

/// Say a library-semantics notice to the person watching the scan. A wait
/// holds its notice only while the returned guard lives, so the line stops
/// saying "waiting up to 30 s" once the ask has come back (carrick#1674);
/// where a schedule ended stands until something replaces it.
fn say_schedule(
    notice: crate::client_semantics::ScheduleNotice,
) -> Option<crate::progress::WaitNotice> {
    if notice.is_wait() {
        Some(crate::progress::announce_wait(notice.line()))
    } else {
        crate::progress::announce(&notice.line());
        None
    }
}

/// A detection an earlier pass of this service already has, with the
/// extraction config asked alongside it (when that answered).
struct SettledDetection {
    detection: DetectionResult,
    extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
}

/// Whether a persisted guidance can be replayed instead of asked for again.
///
/// The id is the whole test. `/analyze-file` sends it so the cloud's analysis
/// cache can key the guidance block by identity rather than by its text, and a
/// guidance without one puts its TEXT in the key instead — so every file in the
/// service re-pays whenever the words move, which is the recurring cost the id
/// was built to remove (carrick-cloud#871).
///
/// A blob written before the id existed carries none, and the
/// `package_json_hash` gate around the replay does not move on an ordinary
/// scan: without this check such a repo replays keyless guidance on every scan
/// from here on and never self-heals (carrick#1224). Asking again costs the
/// five guidance calls once — the cloud serves them from its own guidance
/// cache — and the answer that comes back carries the id, which the blob then
/// persists.
fn guidance_is_keyed(guidance: &ProtocolGuidance) -> bool {
    !guidance.is_empty() && guidance.values().all(|g| g.guidance_key.is_some())
}

impl SettledDetection {
    /// The detection `prev` kept when its guidance was deferred
    /// ([`ModelSetup::guidance_deferred`]) or cannot be replayed
    /// ([`guidance_is_keyed`]): there is a cached detection and no usable
    /// cached guidance, from this cache version and these manifests. A
    /// complete previous generation returns `None`; the incremental branch
    /// reuses it whole, and a full analysis asks again as it always has.
    fn kept_by(prev: &CloudRepoData, current_pkg_hash: &str) -> Option<Self> {
        if prev.cached_guidance.as_ref().is_some_and(guidance_is_keyed)
            || prev.cache_version != Some(CACHE_VERSION)
            || prev.package_json_hash.as_deref() != Some(current_pkg_hash)
        {
            return None;
        }
        prev.cached_detection.clone().map(|detection| Self {
            detection,
            extraction_config: prev.cached_extraction_config.clone(),
        })
    }
}

/// Run framework detection, per-protocol guidance generation, and
/// extraction-config generation (machinery-unwrap rules). All three are
/// cached together under the package_json_hash gate.
///
/// `settled` is a detection the service already holds from a pass whose
/// guidance failed: detection is not asked again, only guidance (and the
/// extraction config, when that failed too).
///
/// Never fails. Detection and guidance are single calls a whole service
/// depends on, so they retry under [`RetryPolicy::PATIENT`]; when even that is
/// spent, the service's model analysis is DEFERRED rather than the run ended:
/// its files are analysed facts-only, none of the missing answers is cached,
/// and the engine names it at the end and asks again (2026-09-15: one
/// exhausted detection call aborted a seven-service first index after four
/// services were done). A guidance failure keeps the detection that answered,
/// so the next ask is guidance only (carrick#1126).
///
/// A deferred service is never analysed under a stand-in guidance. The
/// analyzer's cache key names the guidance it embedded, so answers bought
/// under a placeholder would be paid for again the moment the real guidance
/// arrived.
async fn model_setup(
    packages: &Packages,
    import_facts: &crate::framework_detector::ImportSample,
    settled: Option<SettledDetection>,
) -> ModelSetup {
    crate::scan_stage::enter(crate::scan_stage::Stage::FrameworkDetect);
    let patient = AgentService::new().with_retry_policy(RetryPolicy::PATIENT);
    let (detection, settled_extraction_config) = match settled {
        Some(settled) => {
            debug!("Reusing this service's detection; asking for its guidance only");
            (settled.detection, settled.extraction_config)
        }
        None => match FrameworkDetector::new(patient.clone())
            .detect_frameworks_and_libraries(packages, import_facts)
            .await
        {
            Ok(detection) => (detection, None),
            Err(error) => return ModelSetup::deferred("framework detection", error.as_ref()),
        },
    };

    let guidance_agent = FrameworkGuidanceAgent::new(patient);
    // The extraction config is non-fatal and asked under the ordinary policy:
    // a service that goes without it keeps machinery types wrapped, which is
    // not worth ten minutes of waiting.
    let extraction_agent = FrameworkGuidanceAgent::new(AgentService::new());
    // Guidance and extraction config both depend only on detection — run
    // them concurrently instead of paying a lone extra lambda round-trip.
    let (guidance, extraction_config) = tokio::join!(
        guidance_agent.generate_for_active_protocols(&detection),
        async {
            match settled_extraction_config {
                Some(config) => Some(config),
                None => generate_extraction_config(&extraction_agent, &detection, packages).await,
            }
        },
    );
    match guidance {
        Ok(guidance) => ModelSetup::ready(detection, guidance, extraction_config),
        Err(error) => ModelSetup::guidance_deferred(detection, extraction_config, error.as_ref()),
    }
}

/// Generate machinery-unwrap rules via the cloud's extraction_config task.
/// Non-fatal: on failure the scan proceeds without unwrapping (machinery
/// types like `AxiosResponse<T>` stay wrapped in the manifest).
async fn generate_extraction_config(
    agent: &FrameworkGuidanceAgent,
    detection: &DetectionResult,
    packages: &Packages,
) -> Option<crate::services::type_sidecar::ExtractionConfig> {
    // Local mode makes no model call at all, and this is one (carrick#708).
    // Without the gate a laptop scan fires it, fails on the missing OIDC
    // credential, and prints GitHub Actions advice to somebody who is not in
    // CI — while the scan proceeds without unwrapping either way.
    if crate::local_mode::no_model() {
        debug!("Local mode: skipping extraction-config generation (no model)");
        return None;
    }
    let dependencies = packages.cleaned_dependency_names();
    match agent
        .fetch_extraction_config(detection, &dependencies)
        .await
    {
        Ok(config) => {
            debug!(
                "Extraction config generated: {} unwrap rule(s)",
                config.rules.len()
            );
            Some(config)
        }
        Err(e) => {
            warn!(
                "Extraction-config generation failed: {} — machinery wrapper types will not \
                 be unwrapped this run",
                e
            );
            None
        }
    }
}

/// Append deterministically extracted protocol operations to the repo's
/// index data: GraphQL (SDL root fields as endpoints, document top-level
/// fields as calls), Socket.IO (listeners as endpoints, emitters as calls),
/// and the in-process event bus (subscribers as endpoints, emissions as
/// calls). These protocols never go through the LLM pipeline.
/// Deterministically-extracted non-HTTP operations (GraphQL + Socket.IO +
/// in-process event bus).
/// Returned by `append_deterministic_protocol_operations` so the same scan
/// feeds both `cloud_data.endpoints/calls` and the type manifest
/// (`append_protocol_manifest_entries`) without scanning the files twice.
#[derive(Default)]
struct ProtocolExtractions {
    graphql: crate::graphql::GraphqlExtraction,
    sockets: crate::socket_io::SocketExtraction,
    /// In-process EventEmitter contracts (carrick#676). Scanned after
    /// `sockets`, which it needs to know which spans are already socket ops.
    event_bus: crate::event_emitter::BusExtraction,
    /// Broker, socket and in-process rows read through verified library
    /// claims (carrick#1662, [`library_rows`]). Read once the request inputs
    /// and the sidecar are at hand, after the scan above.
    library: crate::library_claims::LibraryRows,
}

/// The directories to walk for a service's own GraphQL SDL files: its
/// `directory` (or the repo root for a flat single-service config) plus any
/// `include` roots. Mirrors `find_service_files`' scoping so a monorepo
/// package's schema is attributed only to the package that declares it, not its
/// siblings (#242).
fn service_graphql_roots(repo_path: &str, service: &Config) -> Vec<PathBuf> {
    let root = Path::new(repo_path);
    let mut roots = vec![match &service.directory {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    }];
    for inc in &service.include {
        roots.push(root.join(inc));
    }
    roots
}

/// Run the deterministic protocol scans (GraphQL SDL/documents, Socket.IO)
/// and join in the file-analyzer's located producer types. Split from
/// `append_deterministic_protocol_operations` so the extractions exist BEFORE
/// the mount graph is projected into cloud data — the GraphQL consumer file
/// set drives `fold_graphql_transport_calls` on the graph first (#307).
///
/// The calls that execute a GraphQL document are read here too, and placed
/// in the extraction by [`settle_graphql_documents`] once the transport fold
/// has run (carrick#1157). The located consumer types join there, after the
/// placement, because they are keyed by the file that executes the document.
fn scan_protocol_extractions(
    repo_path: &str,
    service: &Config,
    files: &[PathBuf],
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    socket_clients: &[String],
) -> (
    ProtocolExtractions,
    crate::graphql_document_sites::DocumentSiteConsumers,
) {
    let scan_roots = service_graphql_roots(repo_path, service);
    // The printed schemas the service declares it serves (carrick#1099). What
    // they failed to declare is reported once the run's rows are built
    // (`graphql_schema_notices`); here only the files matter.
    let declared =
        crate::graphql::resolve_declared_schemas(Path::new(repo_path), &service.graphql_schemas);
    let mut graphql = crate::graphql::scan_repo(&scan_roots, &declared.files, files);
    merge_graphql_resolver_locations(&mut graphql, file_results);
    // Aliases resolve here as they do for the HTTP-twin drop: a page imports
    // its generated documents through the repo's path aliases as often as
    // through a relative specifier.
    let workspace =
        crate::workspace_resolver::WorkspaceIndex::build_with_aliases(Path::new(repo_path), None);
    let document_sites =
        crate::graphql_document_sites::collect_document_site_consumers(files, Some(&workspace));
    // The detected socket clients gate the pass's unknown-direction half; its
    // Socket.IO rules are independent of them (carrick#1281).
    let sockets = crate::socket_io::scan_files(files, socket_clients);
    let event_bus = crate::event_emitter::scan_files(files, &sockets);
    (
        ProtocolExtractions {
            graphql,
            sockets,
            event_bus,
            library: crate::library_claims::LibraryRows::default(),
        },
        document_sites,
    )
}

/// A service's library sites and what the library store is asked about
/// them (carrick#1664). Read before the analysis, so the first ask runs
/// beside it.
struct LibraryAsk {
    sites: crate::request_summary::LibrarySites,
    asked: crate::library_store::Asked,
}

impl LibraryAsk {
    /// What the service asks, or `None` when it asks nothing: a run with no
    /// model asks the cloud nothing, a scan with no sidecar could verify no
    /// answer, and a service whose library calls go through no public
    /// registry package has nothing to ask about.
    fn start(
        inputs: &crate::request_summary::RequestSummaryInputs,
        sidecar: Option<&TypeSidecar>,
        service_root: &Path,
        repo_root: &Path,
    ) -> Option<Self> {
        if crate::local_mode::no_model() || sidecar.is_none() {
            return None;
        }
        let sites = crate::request_summary::library_sites(inputs);
        let specifiers: BTreeSet<String> = sites.packages().into_values().flatten().collect();
        let home = dirs::home_dir();
        let yarn_registry_env = std::env::var("YARN_NPM_REGISTRY_SERVER").ok();
        let npm_registry_env = std::env::var("NPM_CONFIG_REGISTRY").ok();
        let asked = crate::library_store::request(
            &specifiers,
            &crate::library_store::Install {
                service_root,
                repo_root,
                home: home.as_deref(),
                yarn_registry_env: yarn_registry_env.as_deref(),
                npm_registry_env: npm_registry_env.as_deref(),
            },
        )?;
        Some(Self { sites, asked })
    }
}

/// Send one request to the library store: one attempt, because a failure
/// costs nothing the next scan does not fix, and a refusal or a throttle
/// means no claims this scan.
async fn send_to_library_store(
    request: crate::library_store::LibraryClaimsRequest,
) -> Result<String, crate::agent_service::AgentCallError> {
    reask_agent()
        .post_to_lambda(
            crate::library_store::ROUTE,
            &request,
            crate::library_store::MOCK_SEED,
        )
        .await
}

/// The store's first answer for `ask`, asked beside the analysis.
async fn first_library_answer(ask: Option<&LibraryAsk>) -> crate::library_store::LibraryAnswer {
    match ask {
        Some(ask) => {
            crate::library_store::ask(ask.asked.request.clone(), send_to_library_store).await
        }
        None => crate::library_store::LibraryAnswer::default(),
    }
}

/// The service's library rows: its library sites read through the claims
/// the store answered (packages it had pending asked once more), each
/// verified by the sidecar from `from_dir`. Nothing asked reads nothing.
async fn library_rows(
    ask: Option<&LibraryAsk>,
    first: crate::library_store::LibraryAnswer,
    sidecar: Option<&TypeSidecar>,
    from_dir: &Path,
) -> crate::library_claims::LibraryRows {
    let Some(ask) = ask else {
        return crate::library_claims::LibraryRows::default();
    };
    let claims = crate::library_store::settle(&ask.asked, first, send_to_library_store).await;
    read_library_rows(&ask.sites, &claims, sidecar, from_dir)
}

/// `sites` read through `claims`, each verified by the sidecar against the
/// package's own declarations, from `from_dir` (the service root, as for the
/// client semantics). Verification runs on every scan and is never cached.
/// No claims, or no sidecar, read nothing.
fn read_library_rows(
    sites: &crate::request_summary::LibrarySites,
    claims: &[crate::library_claims::ExportClaims],
    sidecar: Option<&TypeSidecar>,
    from_dir: &Path,
) -> crate::library_claims::LibraryRows {
    let Some(sidecar) = sidecar.filter(|_| !claims.is_empty()) else {
        return crate::library_claims::LibraryRows::default();
    };
    crate::library_claims::read(sites, claims, |checks| {
        sidecar
            .verify_library_claims(from_dir, checks)
            .map(|answer| answer.verdicts)
            .map_err(|error| error.to_string())
    })
}

/// Withdraw every model route stated at exactly the span of a definition
/// read through a verified library claim (carrick#1662): the call
/// registers a handler under a name, which the library row states, and is
/// no HTTP route. Fails closed: a model route at any other span, a route a
/// pass stated, or a definition the scan cannot place keeps its row.
fn withdraw_model_routes_at_definitions(
    mount_graph: &mut crate::mount_graph::MountGraph,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    library: &crate::library_claims::LibraryRows,
    repo_path: &str,
) -> usize {
    let repo_root = normalize_protocol_file(Path::new(repo_path));
    let relative = |file: &Path| {
        let file = normalize_protocol_file(file);
        file.strip_prefix(&repo_root)
            .map(Path::to_path_buf)
            .unwrap_or(file)
    };
    let definitions: HashSet<(PathBuf, u32, u32)> = library
        .rows
        .iter()
        .filter(|row| row.definition)
        .map(|row| (relative(&row.file), row.span_start, row.span_end))
        .collect();
    if definitions.is_empty() {
        return 0;
    }
    let mut withdrawn: HashSet<(String, String, String)> = HashSet::new();
    for (path, result) in file_results {
        let file = relative(Path::new(path));
        for endpoint in &result.endpoints {
            let (Some(start), Some(end)) = (
                endpoint.call_expression_span_start,
                endpoint.call_expression_span_end,
            ) else {
                continue;
            };
            if definitions.contains(&(file.clone(), start, end)) {
                withdrawn.insert((
                    format!("{}:{}", file.display(), endpoint.line_number),
                    endpoint.method.to_uppercase(),
                    endpoint.path.clone(),
                ));
            }
        }
    }
    let before = mount_graph.endpoints.len();
    mount_graph.endpoints.retain(|endpoint| {
        endpoint.resolution_source
            != Some(crate::agents::file_analyzer_agent::ResolutionSource::Model)
            || !withdrawn.contains(&(
                // `file:line`, relative as the analysis keys are: the full
                // scan's graph holds the walked path, absolute when the repo
                // path is.
                relative(Path::new(&endpoint.file_location))
                    .display()
                    .to_string(),
                endpoint.method.to_uppercase(),
                endpoint.path.clone(),
            ))
    });
    before - mount_graph.endpoints.len()
}

/// Attribute a service's GraphQL documents to schema identities, fold the
/// transport calls of every document file out of the graph, then drop the
/// operations of documents written against a schema no service here serves or
/// whose schema cannot be settled (carrick#1134).
///
/// A document against someone else's API is not a call this project can
/// match, and indexing it as one reads as a missing operation whenever the
/// project serves any GraphQL at all. HTTP calls to a declared external base
/// are not call rows either; this is the GraphQL half of that rule, decided by
/// the schema because a vendor API proxied through an internal route has an
/// internal base URL.
///
/// The fold runs on the full document set, before the drop, so a vendor
/// document's transport POST does not come back as an HTTP call. It runs
/// before the rows at executing calls are placed (carrick#1157): a page that
/// passes a generated document to a hook is not a document file, and folding
/// its HTTP calls would drop the REST requests it also makes. Those rows are
/// attributed like any other document.
///
/// The consumer types the file-analyzer located (`file_results`) join once
/// those rows are placed (carrick#1728). The model answers for the file it
/// reads, and the row a located type describes sits in that file only after
/// the placement: before it, the operation is still at its document file or
/// the module that declares the document, and no locate finds it. A located
/// type that is the result of the row's whole operation is then read at the
/// row's field (carrick#1760); `modules` resolves the specifier it was located
/// through.
#[allow(clippy::too_many_arguments)]
fn settle_graphql_documents(
    graphql: &mut crate::graphql::GraphqlExtraction,
    document_sites: crate::graphql_document_sites::DocumentSiteConsumers,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
    modules: &crate::workspace_resolver::WorkspaceIndex,
    mount_graph: &mut crate::mount_graph::MountGraph,
    service: &Config,
    catalogue: &crate::graphql::SchemaCatalogue,
) {
    fold_graphql_transport_calls(mount_graph, graphql);
    document_sites.apply(graphql);
    merge_graphql_consumer_locations(graphql, file_results, repo_path);
    let read = crate::graphql_document_sites::read_located_result_types(graphql, modules);
    if read > 0 {
        debug!(
            "GraphQL consumer rows typed at their field from a located operation result type: {read}"
        );
    }
    let label = service.service_name.as_deref().unwrap_or("(root)");
    // A schema the service's walk found is one it serves only with evidence
    // that it serves a schema at all (carrick#1189): it declares one, it serves
    // HTTP routes, or the model joined a resolver or backing type to a field.
    // A client app with a vendor schema copied into its tree has none of
    // these. Any HTTP route is broader than a GraphQL server (a backend-for-
    // frontend with a vendor copy has one), but it keeps an SDL-first server's
    // own fields when its resolver join was dropped; carrick#1213 replaces it
    // with a detected GraphQL-server signal.
    let serves_schema = !service.graphql_schemas.is_empty()
        || !mount_graph.endpoints.is_empty()
        || graphql
            .producers
            .iter()
            .any(|producer| producer.resolver_file.is_some());
    let unserved = catalogue.settle_walked_schemas(label, graphql, serves_schema);
    if unserved > 0 {
        info!(
            "GraphQL schema fields in {label}: {unserved} walked from its directory are not \
             served by it (no graphqlSchemas, no HTTP route, no resolver); read as another \
             API's schema"
        );
    }
    let attribution = catalogue.attribute(graphql, |document_file| {
        graphql_document_transport(service, document_file)
    });
    let summary = attribution.apply(graphql);
    if !summary.is_empty() {
        info!(
            "GraphQL documents in {label}: {} operation(s) written against schemas no service \
             serves, {} unresolved; not indexed as calls",
            summary.external.values().sum::<usize>(),
            summary.unresolved
        );
    }
    catalogue.record(label, summary);
}

/// What a GraphQL document file's environment reads say about where it sends
/// its documents: the tie-break for [`settle_graphql_documents`], asked only
/// when a served and an external schema both hold a document's fields.
///
/// The source is the file's own `process.env` / `import.meta.env` reads
/// ([`crate::graphql::file_env_reads`]), not the transport call row: a call
/// that names its document binding has its target rewritten to the operation
/// key by the #361 repair and never reaches the graph as an HTTP call. Every
/// read the service's `internalEnvVars` / `externalEnvVars` classify must
/// agree; a file whose reads are unclassified, mixed or absent says nothing.
/// A base read in another module (an imported config object) is therefore
/// unknown.
fn graphql_document_transport(
    service: &Config,
    document_file: &Path,
) -> crate::graphql::TransportOrigin {
    use crate::graphql::TransportOrigin;
    let mut origin: Option<TransportOrigin> = None;
    for name in crate::graphql::file_env_reads(document_file) {
        let this = if service.internal_env_vars.contains(&name) {
            TransportOrigin::Internal
        } else if service.is_external_env_var(&name) {
            TransportOrigin::External
        } else {
            continue;
        };
        match origin {
            None => origin = Some(this),
            Some(seen) if seen == this => {}
            Some(_) => return TransportOrigin::Unknown,
        }
    }
    origin.unwrap_or(TransportOrigin::Unknown)
}

/// #307 (class 2): drop LLM HTTP data calls that are the TRANSPORT of
/// deterministically-extracted GraphQL consumer operations — one contract must
/// not be indexed twice. A file whose `gql` documents produced consumer ops
/// executes them over a POST to the client's endpoint URL, which the
/// file-analyzer also reports as an HTTP data call (`POST ${GQL_URL}/graphql`);
/// the document ops are the real modeled contract, so the transport call is
/// folded into them. Only env-templated / absolute-URL targets are folded: a
/// plain relative literal path in the same file is a distinct same-origin REST
/// call and is kept. The shape test reads the RAW `target_url`, not
/// `canonical_path` — `consumer_call_path` strips a declared-internal env-var
/// base (`${GQL_URL}/graphql` → `/graphql`), which would otherwise let the
/// transport leak for exactly the users who configured `internalEnvVars`.
/// Known limitation (logged): a REST call built on a DIFFERENT env-var base
/// inside a gql-consumer file is folded too — accepted over leaking a phantom
/// HTTP contract for every GraphQL client file.
fn fold_graphql_transport_calls(
    mount_graph: &mut crate::mount_graph::MountGraph,
    graphql: &crate::graphql::GraphqlExtraction,
) {
    if graphql.consumers.is_empty() {
        return;
    }
    // Normalize both sides component-wise so a `./`-prefixed walk path and a
    // bare file key still join (`components()` keeps a LEADING CurDir, so it
    // is skipped explicitly).
    let normalize_file = |p: &Path| -> PathBuf {
        p.components()
            .skip_while(|c| matches!(c, std::path::Component::CurDir))
            .collect()
    };
    let consumer_files: HashSet<PathBuf> = graphql
        .consumers
        .iter()
        .map(|op| normalize_file(&op.file_path))
        .collect();
    mount_graph.data_calls.retain(|call| {
        let raw = call.target_url.trim_matches(['`', '"', '\'']);
        let is_transport_shape = raw.contains("${")
            || raw.contains("process.env.")
            || raw.starts_with("http://")
            || raw.starts_with("https://");
        if !is_transport_shape {
            return true;
        }
        let Some((file, _line)) = call.file_location.rsplit_once(':') else {
            return true;
        };
        let file_norm = normalize_file(Path::new(file));
        if consumer_files.contains(&file_norm) {
            debug!(
                "Folding GraphQL transport call {} {} ({}) into its document operations",
                call.method, call.canonical_path, file
            );
            false
        } else {
            true
        }
    });
}

/// Where a GraphQL, socket or pub/sub producer row's evidence comes from
/// (#380), classified as an HTTP route's is
/// ([`crate::file_finder::endpoint_provenance`]): from the row's file relative
/// to the service's own directory, so a scan root that itself sits under a
/// conventionally named directory is not misread (carrick#1626).
///
/// Both scan paths are read: the full scan hands over paths as scanned,
/// prefixed by `repo_path`; the incremental scan's pub/sub keys are already
/// repo-relative. A path that is neither (outside the repo) reads as `Route`,
/// the classifier's own conservative answer.
fn protocol_producer_provenance(
    file: &Path,
    repo_path: &str,
    service: &Config,
) -> crate::operation::EndpointProvenance {
    let repo_root = normalize_protocol_file(Path::new(repo_path));
    let file = normalize_protocol_file(file);
    let relative = file
        .strip_prefix(&repo_root)
        .map(Path::to_path_buf)
        .unwrap_or(file);
    if relative.is_absolute() {
        return crate::operation::EndpointProvenance::Route;
    }
    let service_dir =
        normalize_protocol_file(Path::new(service.directory.as_deref().unwrap_or_default()));
    crate::file_finder::endpoint_provenance(&relative, &service_dir)
}

fn append_deterministic_protocol_operations(
    cloud_data: &mut CloudRepoData,
    extractions: &ProtocolExtractions,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    in_process: &crate::in_process_pubsub::InProcessPubsub,
    repo_path: &str,
    service: &Config,
) {
    // Same "{file}:{line}" convention the mount-graph conversions use. A
    // call's row; a producer's is `to_producer` below.
    let to_call = |key: OperationKey, file_path: &Path, line: u32| ApiEndpointDetails {
        owner: None,
        key,
        params: vec![],
        request_body: None,
        response_body: None,
        handler_name: None,
        request_type: None,
        response_type: None,
        file_path: PathBuf::from(format!("{}:{}", file_path.display(), line)),
        repo_name: None,
        service_name: None,
        // Provenance is producer-side metadata, as it is for an HTTP call.
        provenance: Default::default(),
        // These ops come from the deterministic protocol extractions, not from
        // the HTTP emit/join phase, so no pass stated them in the sense
        // `resolution_source` records (carrick#660). The model's pub/sub rows
        // say `model` (`append_pubsub_operations`, carrick#1626).
        resolution_source: None,
        // A file-router module is an HTTP concept; non-HTTP ops carry the
        // default.
        view_module: false,
        // So is body dispatch (carrick#831): a GraphQL field and a socket
        // event are already identified by their own name, and nothing
        // switches on a request field to reach them.
        dispatch: None,
        schema_binding: None,
        // The handler span is placed from an HTTP route's registration
        // (cloud#948); these rows have none.
        handler_span: None,
        name_scope: None,
        library_semantics: Vec::new(),
    };
    // A producer's row: tagged when its file sits in a mock or test-support
    // tree of the service, as an HTTP route is (#380, carrick#1626).
    let to_producer = |key: OperationKey, file_path: &Path, line: u32| ApiEndpointDetails {
        provenance: protocol_producer_provenance(file_path, repo_path, service),
        ..to_call(key, file_path, line)
    };

    let graphql = &extractions.graphql;
    if !graphql.is_empty() {
        debug!(
            producers = graphql.producers.len(),
            consumers = graphql.consumers.len(),
            "Indexing GraphQL operations"
        );
        // A producer is served where its resolver is, when one was located
        // (carrick#1157): the schema file states the field, the resolver is
        // the code that changes when the operation does. A field with only a
        // backing type located has no line to point at and stays on its schema
        // line.
        cloud_data
            .endpoints
            .extend(graphql.producers.iter().map(|op| {
                match (&op.resolver_file, op.resolver_line) {
                    (Some(file), Some(line)) => to_producer(op.key.clone(), file, line),
                    _ => to_producer(op.key.clone(), &op.file_path, op.line),
                }
            }));
        cloud_data
            .calls
            .extend(graphql.consumers.iter().map(|op| ApiEndpointDetails {
                schema_binding: op.schema_binding,
                ..to_call(op.key.clone(), &op.file_path, op.line)
            }));
    }

    let library = append_library_operations(
        cloud_data,
        &extractions.library,
        repo_path,
        service,
        &to_producer,
        &to_call,
    );

    // A socket pass row a library row states at the same file, line, name,
    // direction and side is the same call read twice: the library row is the
    // fact, so the pass's row folds into it. With no library row there, the
    // pass's row stands (ruled on carrick#1664). Its payload anchor stays in
    // the manifest: it types the same operation at the same site.
    let sockets = &extractions.sockets;
    if !sockets.is_empty() {
        let unstated = |ops: &[crate::socket_io::SocketOp], listener: bool| -> Vec<_> {
            ops.iter()
                .filter(|op| !library.states_socket(&op.file_path, op.line, &op.key, listener))
                .cloned()
                .collect()
        };
        let listeners = unstated(&sockets.listeners, true);
        let emitters = unstated(&sockets.emitters, false);
        debug!(
            listeners = listeners.len(),
            emitters = emitters.len(),
            folded =
                sockets.listeners.len() + sockets.emitters.len() - listeners.len() - emitters.len(),
            "Indexing Socket.IO operations"
        );
        cloud_data.endpoints.extend(
            listeners
                .iter()
                .map(|op| to_producer(op.key.clone(), &op.file_path, op.line)),
        );
        cloud_data.calls.extend(
            emitters
                .iter()
                .map(|op| to_call(op.key.clone(), &op.file_path, op.line)),
        );
    }
    append_event_bus_operations(
        cloud_data,
        &extractions.event_bus,
        file_results,
        &library,
        &to_producer,
        &to_call,
    );
    append_pubsub_operations(
        cloud_data,
        file_results,
        &extractions.sockets,
        in_process,
        &library,
        &to_producer,
        &to_call,
    );
}

/// Where library rows were stated (carrick#1662), by normalized file and
/// line: each name, each pub/sub name with its role, and each socket name
/// with its direction and side. A model pub/sub row at the same file, line,
/// topic and role is the same call read again and folds into the library
/// row; so does an event-bus row at the same file, line and name, and a
/// socket pass row at the same file, line, name, direction and side
/// (carrick#1664).
#[derive(Debug, Default)]
struct LibrarySiteIndex {
    /// Files are keyed relative to this root, whichever form a caller holds.
    repo_root: PathBuf,
    named: HashSet<(PathBuf, u32, String)>,
    pubsub: HashSet<(PathBuf, u32, String, crate::operation::PubsubRole)>,
    /// Each socket name with its direction, and whether the row listens.
    socket: HashSet<(
        PathBuf,
        u32,
        String,
        crate::operation::SocketDirection,
        bool,
    )>,
}

impl LibrarySiteIndex {
    fn of(library: &crate::library_claims::LibraryRows, repo_path: &str) -> Self {
        use crate::library_claims::LibraryRowKind;
        let mut index = LibrarySiteIndex {
            repo_root: normalize_protocol_file(Path::new(repo_path)),
            ..LibrarySiteIndex::default()
        };
        for row in &library.rows {
            let file = index.relative(&row.file);
            index
                .named
                .insert((file.clone(), row.line, row.name.clone()));
            match row.kind {
                LibraryRowKind::Pubsub(role) => {
                    index
                        .pubsub
                        .insert((file, row.line, row.name.clone(), role));
                }
                LibraryRowKind::Socket {
                    direction,
                    listener,
                } => {
                    index
                        .socket
                        .insert((file, row.line, row.name.clone(), direction, listener));
                }
            }
        }
        index
    }

    fn relative(&self, file: &Path) -> PathBuf {
        let file = normalize_protocol_file(file);
        file.strip_prefix(&self.repo_root)
            .map(Path::to_path_buf)
            .unwrap_or(file)
    }

    fn states_pubsub(
        &self,
        file: &Path,
        line: u32,
        name: &str,
        role: crate::operation::PubsubRole,
    ) -> bool {
        self.pubsub
            .contains(&(self.relative(file), line, name.to_string(), role))
    }

    /// Whether a library socket row on the same side (listening or not) was
    /// stated here for `key`'s event: in `key`'s direction, or, when the pass
    /// could not read one (`Unknown`), in either (ruled on carrick#1664: the
    /// library row's direction wins).
    fn states_socket(&self, file: &Path, line: u32, key: &OperationKey, listener: bool) -> bool {
        use crate::operation::SocketDirection;
        let OperationKey::Socket { event, direction } = key else {
            return false;
        };
        let directions: &[SocketDirection] = match direction {
            SocketDirection::Unknown => &[
                SocketDirection::ClientToServer,
                SocketDirection::ServerToClient,
            ],
            known => std::slice::from_ref(known),
        };
        let file = self.relative(file);
        directions.iter().any(|direction| {
            self.socket
                .contains(&(file.clone(), line, event.clone(), *direction, listener))
        })
    }

    /// Whether `op` is a socket pass op of unknown direction folded into a
    /// library row here: the operation it named is gone, so an anchor for it
    /// would orphan (the library row's direction replaced its key).
    fn folds_unknown(&self, op: &crate::socket_io::SocketOp, listener: bool) -> bool {
        matches!(
            op.key,
            OperationKey::Socket {
                direction: crate::operation::SocketDirection::Unknown,
                ..
            }
        ) && self.states_socket(&op.file_path, op.line, &op.key, listener)
    }

    /// `sockets` without the ops [`Self::folds_unknown`] names: the ops
    /// whose payload anchors still type an operation. An op folded in its own
    /// direction keeps its anchor, which types the library row on its key.
    fn typed_sockets(
        &self,
        sockets: &crate::socket_io::SocketExtraction,
    ) -> crate::socket_io::SocketExtraction {
        crate::socket_io::SocketExtraction {
            listeners: sockets
                .listeners
                .iter()
                .filter(|op| !self.folds_unknown(op, true))
                .cloned()
                .collect(),
            emitters: sockets
                .emitters
                .iter()
                .filter(|op| !self.folds_unknown(op, false))
                .cloned()
                .collect(),
        }
    }

    fn states_name(&self, file: &Path, line: u32, name: &str) -> bool {
        self.named
            .contains(&(self.relative(file), line, name.to_string()))
    }
}

/// The library rows (carrick#1662): a pub/sub subscriber or a socket
/// listener is a producer, a publisher or an emitter a call, each a
/// `library_claim` fact carrying its claim ids and its name's scope. A row in
/// a mock or test tree of the service states nothing. Returns where the rows
/// that were stated sit.
fn append_library_operations(
    cloud_data: &mut CloudRepoData,
    library: &crate::library_claims::LibraryRows,
    repo_path: &str,
    service: &Config,
    to_producer: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
    to_call: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
) -> LibrarySiteIndex {
    use crate::library_claims::LibraryRowKind;
    use crate::operation::PubsubRole;
    let stated = stated_library_rows(library, repo_path, service);
    for row in &stated.rows {
        let claimed = |details: ApiEndpointDetails| ApiEndpointDetails {
            resolution_source: Some(
                crate::agents::file_analyzer_agent::ResolutionSource::LibraryClaim,
            ),
            name_scope: Some(row.wire_scope()),
            library_semantics: row.claim_ids.clone(),
            ..details
        };
        match row.kind {
            LibraryRowKind::Pubsub(PubsubRole::Subscriber) => cloud_data.endpoints.push(claimed(
                to_producer(OperationKey::pubsub(row.name.clone()), &row.file, row.line),
            )),
            LibraryRowKind::Pubsub(PubsubRole::Publisher) => cloud_data.calls.push(claimed(
                to_call(OperationKey::pubsub(row.name.clone()), &row.file, row.line),
            )),
            LibraryRowKind::Socket {
                direction,
                listener: true,
            } => cloud_data.endpoints.push(claimed(to_producer(
                OperationKey::socket(row.name.clone(), direction),
                &row.file,
                row.line,
            ))),
            LibraryRowKind::Socket {
                direction,
                listener: false,
            } => cloud_data.calls.push(claimed(to_call(
                OperationKey::socket(row.name.clone(), direction),
                &row.file,
                row.line,
            ))),
        }
    }
    if !stated.rows.is_empty() {
        debug!(
            rows = stated.rows.len(),
            "Indexing library-claim operations (carrick#1662)"
        );
    }
    LibrarySiteIndex::of(&stated, repo_path)
}

/// The library rows the service states: every one outside a mock or test
/// tree of the service (design section 9; the tree rule is the one a
/// producer's provenance reads, carrick#380).
fn stated_library_rows(
    library: &crate::library_claims::LibraryRows,
    repo_path: &str,
    service: &Config,
) -> crate::library_claims::LibraryRows {
    crate::library_claims::LibraryRows {
        rows: library
            .rows
            .iter()
            .filter(|row| !protocol_producer_provenance(&row.file, repo_path, service).is_mock())
            .cloned()
            .collect(),
    }
}

/// The model's pub/sub rows that are calls into an in-process wrapper with
/// nothing on the other side of the topic in this service (carrick#1513),
/// read by [`append_pubsub_operations`] and [`append_pubsub_manifest_entries`]
/// so a withdrawn row leaves neither an operation nor an anchor. The rule is
/// [`crate::in_process_pubsub`]'s.
///
/// A transport is whatever framework detection classed as a messaging,
/// socket or data-fetching client. The in-process event-bus pass's rows count
/// as the other side of a topic exactly as the model's own do.
fn classify_in_process_pubsub(
    repo_path: &str,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    cloud_data: &CloudRepoData,
    modules: &crate::workspace_resolver::WorkspaceIndex,
    detection: &DetectionResult,
    event_bus: &crate::event_emitter::BusExtraction,
) -> crate::in_process_pubsub::InProcessPubsub {
    use crate::operation::PubsubRole;

    let transports: Vec<String> = detection
        .messaging_clients
        .iter()
        .chain(&detection.socket_clients)
        .chain(&detection.data_fetchers)
        .cloned()
        .collect();
    let other_sides: Vec<(String, PubsubRole)> = event_bus
        .subscribers
        .iter()
        .map(|op| (op.event.clone(), PubsubRole::Subscriber))
        .chain(
            event_bus
                .publishers
                .iter()
                .map(|op| (op.event.clone(), PubsubRole::Publisher)),
        )
        .collect();
    let classified = crate::in_process_pubsub::classify(
        Path::new(repo_path),
        file_results,
        &cloud_data.function_definitions,
        modules,
        &transports,
        &other_sides,
    );
    if !classified.is_empty() {
        debug!(
            withdrawn = classified.len(),
            "pub/sub rows withdrawn as in-process with no counterpart (carrick#1513)"
        );
    }
    crate::scan_health::record_in_process_pubsub(classified.len());
    classified
}

/// Fold the deterministic in-process event-bus scan into `cloud_data`
/// (carrick#676): a subscription registers a handler and is the contract
/// producer → `cloud_data.endpoints`; an emission sends and is the consumer →
/// `cloud_data.calls`. Identity is the event name alone
/// (`OperationKey::pubsub`), the same key a broker topic uses, because an
/// in-process bus is pub/sub with a shorter wire.
///
/// Where the file-analyzer ALREADY reported a pub/sub op for the same file,
/// topic and role, that row wins and this one is dropped. Both rows describe
/// one call site on one channel, so indexing both would double it — and of the
/// two the model's is the richer: it carries the payload anchor that gives the
/// op a resolved type, which this pass does not yet extract (#688). The value
/// added here is the sites the model reported nothing for, which is the whole
/// of the gap #676 was filed for.
///
/// The tradeoff that accepts: where the model reported a role and the AST
/// disagrees, the model's role stands. It applies only where both saw the same
/// site, and #688 (payload anchors here) is what would let this prefer the AST.
fn append_event_bus_operations(
    cloud_data: &mut CloudRepoData,
    event_bus: &crate::event_emitter::BusExtraction,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    library: &LibrarySiteIndex,
    to_producer: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
    to_call: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
) {
    use crate::operation::PubsubRole;

    if event_bus.is_empty() {
        return;
    }
    let reported = llm_pubsub_sites(file_results);
    let mut subscribers = 0usize;
    let mut publishers = 0usize;
    let mut deferred = 0usize;
    let mut push = |ops: &[crate::event_emitter::BusOp],
                    role: PubsubRole,
                    to_details: &dyn Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
                    into: &mut Vec<ApiEndpointDetails>,
                    counter: &mut usize| {
        for op in ops {
            // A library row stated this call (carrick#1662).
            if library.states_name(&op.file_path, op.line, &op.event) {
                deferred += 1;
                continue;
            }
            let site = (
                normalize_protocol_file(&op.file_path),
                op.event.clone(),
                role,
            );
            if reported.contains(&site) {
                debug!(
                    event = %op.event,
                    file = %op.file_path.display(),
                    "event bus op deferred to the file-analyzer's row for the same site"
                );
                deferred += 1;
                continue;
            }
            into.push(to_details(op.key.clone(), &op.file_path, op.line));
            *counter += 1;
        }
    };
    push(
        &event_bus.subscribers,
        PubsubRole::Subscriber,
        to_producer,
        &mut cloud_data.endpoints,
        &mut subscribers,
    );
    push(
        &event_bus.publishers,
        PubsubRole::Publisher,
        to_call,
        &mut cloud_data.calls,
        &mut publishers,
    );
    debug!(
        subscribers,
        publishers, deferred, "Indexing in-process event bus operations"
    );
}

/// Sites the file-analyzer already reported as pub/sub, as (normalized file,
/// topic, role). Read by [`append_event_bus_operations`] to know which of its
/// own rows would be a second copy of one the model already produced. An op
/// with no role names no site: it was dropped from `cloud_data` entirely, so it
/// covers nothing.
fn llm_pubsub_sites(
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
) -> HashSet<(PathBuf, String, crate::operation::PubsubRole)> {
    let mut sites = HashSet::new();
    for (path, result) in file_results {
        let file_norm = normalize_protocol_file(Path::new(path));
        for op in &result.pubsub_operations {
            if let Some(role) = op.role {
                sites.insert((file_norm.clone(), op.topic.clone(), role));
            }
        }
    }
    sites
}

/// Component-wise path normalization used by the protocol folds: strip a leading
/// `./` (a `CurDir` component) so a walk-derived path (`./realtime/server.ts`)
/// and a repo-relative file_results key (`realtime/server.ts`) collapse to the
/// same value. Mirrors the local normalizer in `fold_graphql_transport_calls`.
fn normalize_protocol_file(p: &Path) -> PathBuf {
    p.components()
        .skip_while(|c| matches!(c, std::path::Component::CurDir))
        .collect()
}

/// Structural fold set: normalized file → the event names for which the
/// deterministic Socket.IO scan already emitted an operation in that file
/// (emitter OR listener). The file-analyzer sometimes reports a single
/// `socket.emit("x", …)` / `socket.on("x", …)` site as BOTH a socket event and
/// a pub/sub op; the deterministic socket op is the modeled contract, so a
/// pub/sub op sharing the SAME file AND the SAME event/topic string is folded
/// away (dropped) in favor of it — otherwise the emit is indexed twice (once
/// `socket|…`, once `pubsub|…`), inflating the call set.
///
/// An in-process bus op does NOT fold anything here: it is on the same channel
/// as the pub/sub row it would replace, and the model's row is the richer of
/// the two (it carries a payload anchor), so the deduplication runs the other
/// way, in `append_event_bus_operations` (carrick#676).
///
/// The match keys purely on structural coincidence (same file + same name),
/// never on a library/broker name, so a genuine Kafka/NATS/Redis/BullMQ publish
/// in a file that has NO socket op on that event is untouched. Requiring the
/// same-file socket twin is what keeps real pub/sub (which lives in files
/// without socket ops) safe.
///
/// Keyed as a map of file → event set (rather than a set of owned pairs) so
/// membership checks borrow `&Path`/`&str` without per-op cloning.
fn socket_event_twins(
    sockets: &crate::socket_io::SocketExtraction,
) -> HashMap<PathBuf, HashSet<String>> {
    let mut twins: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for op in sockets.listeners.iter().chain(sockets.emitters.iter()) {
        if let Some(event) = op.key.socket_event() {
            twins
                .entry(normalize_protocol_file(&op.file_path))
                .or_default()
                .insert(event.to_string());
        }
    }
    twins
}

/// Membership check against [`socket_event_twins`]'s map using borrowed keys.
fn has_socket_twin(
    twins: &HashMap<PathBuf, HashSet<String>>,
    file_norm: &Path,
    topic: &str,
) -> bool {
    twins
        .get(file_norm)
        .is_some_and(|events| events.contains(topic))
}

/// Fold the file-analyzer's `pubsub_operations` into `cloud_data` so they reach
/// the exact-key matcher (#corpus-2 edge #4). A subscriber registers a handler
/// and is the contract producer → `cloud_data.endpoints`; a publisher sends and
/// is the consumer → `cloud_data.calls`. Identity is the topic alone
/// (`OperationKey::pubsub`), so a subscriber and a publisher on the same topic in
/// two repos share one key and match.
///
/// Unlike GraphQL there is no SDL backstop, so we push every LLM op directly
/// (the deterministic append path), NOT through `merge_graphql_resolver_locations`
/// which would discard ops with no schema producer. Repo identity is back-filled
/// later by `AnalyzerBuilder::build_from_repo_data`; the only requirement is that
/// these ops sit in `cloud_data` before serialization.
///
/// An op whose `role` is `None` (model omitted it or emitted an off-enum value,
/// absorbed leniently) can't be placed on either side and is dropped with a debug
/// log. Only literal topics are extracted today (env-template collapse is deferred).
///
/// An op `in_process` withdrew is a call into this repo's own in-process
/// wrapper with no counterpart in the service (carrick#1513): it is not a
/// cross-service event, so it is counted and not indexed.
///
/// A row the model stated carries `resolution_source: model`, so it reads as a
/// candidate everywhere a source is read (carrick#1626). An op the scanner's
/// pass backfilled into the same list (`PubsubOperation::backfilled`) states
/// no source.
fn append_pubsub_operations(
    cloud_data: &mut CloudRepoData,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    sockets: &crate::socket_io::SocketExtraction,
    in_process: &crate::in_process_pubsub::InProcessPubsub,
    library: &LibrarySiteIndex,
    to_producer: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
    to_call: &impl Fn(OperationKey, &Path, u32) -> ApiEndpointDetails,
) {
    use crate::operation::PubsubRole;

    let twins = socket_event_twins(sockets);
    let mut subscribers = 0usize;
    let mut publishers = 0usize;
    let mut dropped = 0usize;
    let mut folded = 0usize;
    let mut withdrawn = 0usize;
    // Deterministic order: HashMap iteration is unordered, so sort by path
    // before pushing endpoints/calls (keeps scanner output stable).
    let mut paths: Vec<&String> = file_results.keys().collect();
    paths.sort();
    for path in paths {
        let result = &file_results[path];
        let file_norm = normalize_protocol_file(Path::new(path));
        for op in &result.pubsub_operations {
            // Same-file socket twin → the file-analyzer double-classified a
            // socket emit/listen site; keep the deterministic socket op, drop
            // this pub/sub form so the site is indexed once.
            if has_socket_twin(&twins, &file_norm, &op.topic) {
                debug!(
                    topic = %op.topic,
                    file = %path,
                    "pub/sub op folded into same-file socket twin"
                );
                folded += 1;
                continue;
            }
            if let Some(role) = op.role
                && in_process.is_withdrawn(Path::new(path), op.line_number, &op.topic, role)
            {
                withdrawn += 1;
                continue;
            }
            let line = u32::try_from(op.line_number).unwrap_or(0);
            // A library row stated this call: same file, line, topic and
            // role (carrick#1662). A model row for the topic at any other
            // line stands.
            if let Some(role) = op.role
                && library.states_pubsub(Path::new(path), line, &op.topic, role)
            {
                folded += 1;
                continue;
            }
            let file_path = PathBuf::from(path);
            let key = OperationKey::pubsub(op.topic.clone());
            // The model's row says so (carrick#1626). A row the scanner's own
            // pass backfilled into the model's list is not the model's, and no
            // pass here is a labelled source yet, so it states none.
            let stated = |row: ApiEndpointDetails| ApiEndpointDetails {
                resolution_source: (!op.backfilled)
                    .then_some(crate::agents::file_analyzer_agent::ResolutionSource::Model),
                ..row
            };
            match op.role {
                Some(PubsubRole::Subscriber) => {
                    cloud_data
                        .endpoints
                        .push(stated(to_producer(key, &file_path, line)));
                    subscribers += 1;
                }
                Some(PubsubRole::Publisher) => {
                    cloud_data
                        .calls
                        .push(stated(to_call(key, &file_path, line)));
                    publishers += 1;
                }
                None => {
                    debug!(
                        topic = %op.topic,
                        file = %path,
                        "pubsub_operation has no role; dropping"
                    );
                    dropped += 1;
                }
            }
        }
    }
    if subscribers + publishers + dropped + folded + withdrawn > 0 {
        debug!(
            subscribers,
            publishers,
            dropped,
            folded,
            withdrawn_in_process = withdrawn,
            "Indexing pub/sub operations"
        );
    }
}

/// Emit type-manifest entries for the LLM-extracted pub/sub operations so they
/// carry a type anchor, mirroring the Socket.IO manifest path exactly (#PR-4).
///
/// `append_pubsub_operations` already places pub/sub ops in
/// `cloud_data.endpoints/calls`, which is enough for the exact-key matcher to
/// MATCH a subscriber against a publisher — but without a manifest entry the op
/// has no `primary_type_symbol` anchor, so the anchor + resolution dimensions
/// treat every extracted pub/sub op as an untyped miss. This re-walks the same
/// `file_results` and, for each op carrying a decoded-payload
/// `primary_type_symbol`, emits one Response-kind manifest entry: a subscriber
/// (the contract producer) → `ManifestRole::Producer`; a publisher (the
/// consumer) → `ManifestRole::Consumer`. The `primary_type_symbol` is threaded
/// straight onto the entry so the sidecar resolves it through the same
/// SymbolRequest bundle path Socket.IO payloads use.
///
/// Pub/sub ops live in `file_results` (LLM-sourced), not the deterministic
/// `ProtocolExtractions` struct, so this is a sibling of
/// `append_protocol_manifest_entries` rather than a branch inside it. It is
/// called at both manifest call sites (incremental + full) right after the
/// deterministic protocols are folded in.
///
/// Mirroring socket's null handling: an op with `primary_type_symbol: None`
/// (untyped or inline-object payload) still gets a manifest entry, just with a
/// `None` symbol — the entry stays `Unknown`, exactly as a socket emitter whose
/// payload type the extractor couldn't capture. An op with no role is skipped
/// (it was already dropped from `cloud_data` and has nothing to anchor).
///
/// A pub/sub op folded away by the same-file socket-twin guard (see
/// `socket_event_twins`) is also skipped here: it was dropped from `cloud_data`
/// by `append_pubsub_operations`, so leaving a manifest anchor for it would
/// orphan the anchor. The `sockets` extraction feeds the same fold set both
/// places, and so does `in_process` (carrick#1513): a row it withdrew has no
/// operation to anchor.
fn append_pubsub_manifest_entries(
    entries: &mut Vec<TypeManifestEntry>,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    sockets: &crate::socket_io::SocketExtraction,
    in_process: &crate::in_process_pubsub::InProcessPubsub,
    library: &LibrarySiteIndex,
    repo_root: &str,
) {
    use crate::operation::PubsubRole;

    let twins = socket_event_twins(sockets);
    // Deterministic order: sort paths before emitting manifest entries.
    let mut paths: Vec<&String> = file_results.keys().collect();
    paths.sort();
    for path in paths {
        let result = &file_results[path];
        let file_norm = normalize_protocol_file(Path::new(path));
        for op in &result.pubsub_operations {
            // Folded into a same-file socket twin: dropped from cloud_data, so
            // emit no orphan anchor here either.
            if has_socket_twin(&twins, &file_norm, &op.topic) {
                continue;
            }
            if let Some(pubsub_role) = op.role
                && in_process.is_withdrawn(Path::new(path), op.line_number, &op.topic, pubsub_role)
            {
                continue;
            }
            // Folded into the library row at the same site (carrick#1662).
            if let Some(pubsub_role) = op.role
                && library.states_pubsub(
                    Path::new(path),
                    u32::try_from(op.line_number).unwrap_or(0),
                    &op.topic,
                    pubsub_role,
                )
            {
                continue;
            }
            let role = match op.role {
                Some(PubsubRole::Subscriber) => ManifestRole::Producer,
                Some(PubsubRole::Publisher) => ManifestRole::Consumer,
                // No role → not placed on either side of `cloud_data`; nothing
                // to anchor, so emit no manifest entry.
                None => continue,
            };
            let key = OperationKey::pubsub(op.topic.clone());
            // Clamp to a valid 1-based line. A degenerate (<= 0) line must still
            // hash identically here and on the SymbolRequest side, and 0 is an
            // invalid anchor everywhere else (`parse_file_location` et al.).
            let line = u32::try_from(op.line_number).unwrap_or(0).max(1);
            // Publishers (consumers) disambiguate by call site. Two repos
            // publishing to the same topic (fan-in — the common event-driven
            // shape) otherwise hash to ONE consumer alias, and the merged
            // consumer declarations then define that interface twice with
            // different bodies — one publisher's payload masks the other's,
            // yielding a spurious compat mismatch on whichever loses. Mirror the
            // HTTP consumer path (`add_manifest_pair` + `build_site_id`).
            // Subscribers (producers) keep the plain alias: one definition per
            // topic per repo, exactly like an HTTP endpoint.
            let call_id = match role {
                ManifestRole::Consumer => Some(build_site_id(path, line, &key, repo_root)),
                ManifestRole::Producer => None,
            };
            add_protocol_manifest_entry(
                entries,
                &key,
                role,
                path,
                line,
                op.primary_type_symbol.clone(),
                call_id.as_deref(),
            );
        }
    }
}

/// Fold the file-analyzer's `graphql_operations` into the SDL-derived producers
/// (Stage B1). The SDL `scan_repo` gives the producer's canonical
/// `OperationKey` and its SDL anchor, but NOT where the resolver lives — and the
/// producer's real response contract is the resolver function's RETURN type
/// expanded (`Promise<ApiResponse<Order>>` → `{ data: …, errors }`), which only
/// a `FunctionReturn` infer at the resolver's file/line can give.
///
/// For each LLM `graphql_operation`, build its canonical `OperationKey` and match
/// it to the SDL producer with the same key; populate that producer's
/// `resolver_file` (the file the op came from — `file_results` is keyed by path)
/// and `resolver_line`. An LLM op with no matching SDL producer is ignored
/// (logged at debug): without an SDL producer there is no manifest entry to join
/// back to, so a resolver location alone is inert.
///
/// Two deterministic guards harden the join against misattributed claims (the
/// corpus-3 `query ticket` live-eval false-incompatible: the model linked a
/// NestJS HTTP handler returning a ticket-SHAPED object as the field's
/// resolver, and its inferred return then rode the producer's type surface
/// into a confidently wrong verdict):
///
/// 1. **HTTP-handler borrow witness**: a claim whose `resolver_function`
///    names a function the SAME file's analysis bound as an HTTP endpoint
///    handler (`endpoints[].handler_name`) is dropped. The witness is the
///    model's own output — a function it identified as the handler of an
///    HTTP route is that route's contract, not a schema field's resolver.
///    Framework-agnostic: no framework names, only structural agreement
///    within one `FileAnalysisResult`.
/// 2. **Cross-claim ambiguity fails closed**: claims are collected over a
///    SORTED file order (never `HashMap` iteration order, which previously
///    let the last writer win nondeterministically), and when two surviving
///    claims for one producer key disagree on `(file, function)` — or two
///    backing-type claims disagree on `(file, symbol)` — the location is
///    dropped entirely. An unlocated producer abstains (type_state stays
///    `Unknown`, its pairs verdict unverifiable) — never a coin-flip between
///    a right and a wrong site.
///
/// A resolver-location claim beats a backing-type claim for the same key
/// (the resolver-first gate, 186cb27), now across files as well as within
/// one op — previously a backing-only claim from a later file could smear
/// its `resolver_file` under another file's `resolver_line`.
///
/// Consumers are never touched — they anchor on `payload_type_symbol`.
fn merge_graphql_resolver_locations(
    graphql: &mut crate::graphql::GraphqlExtraction,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
) {
    // Canonical producer key -> index, so each LLM op joins in O(1) without an
    // N×M scan (keys are unique across producers).
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (idx, op) in graphql.producers.iter().enumerate() {
        by_key.insert(op.key.canonical(), idx);
    }

    // Claims are collected fully before anything is applied, so a conflicting
    // later claim voids the location instead of racing for it.
    // producer idx -> (file, resolver function, 1-based line).
    let mut resolver_claims: HashMap<usize, (String, String, u32)> = HashMap::new();
    let mut resolver_conflicts: HashSet<usize> = HashSet::new();
    // producer idx -> (file, backing symbol, backing source).
    #[allow(clippy::type_complexity)]
    let mut backing_claims: HashMap<usize, (String, String, Option<String>)> = HashMap::new();
    let mut backing_conflicts: HashSet<usize> = HashSet::new();

    let mut paths: Vec<&String> = file_results.keys().collect();
    paths.sort();
    for path in paths {
        let result = &file_results[path];
        if result.graphql_operations.is_empty() {
            continue;
        }
        // Borrow witness evidence: every function name this file's analysis
        // bound as an HTTP endpoint handler.
        let handler_names: HashSet<&str> = result
            .endpoints
            .iter()
            .map(|endpoint| endpoint.handler_name.trim())
            .filter(|name| !name.is_empty())
            .collect();
        for llm_op in &result.graphql_operations {
            let key = OperationKey::graphql(llm_op.kind, llm_op.field.clone());
            let Some(&idx) = by_key.get(&key.canonical()) else {
                debug!(
                    op = %key.canonical(),
                    file = %path,
                    "graphql_operation has no matching SDL producer; ignoring resolver location"
                );
                continue;
            };
            // Trim before the emptiness check: a whitespace-only string (e.g.
            // " ") from the model is not a resolver function name.
            let resolver_name = llm_op
                .resolver_function
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            // Guard 1: the claimed resolver is an HTTP endpoint handler in the
            // same file analysis — the misattribution class. Drop the whole
            // claim (a resolver-named op carries no backing type by schema
            // contract, and a contract-violating one must not smuggle one in).
            if let Some(name) = resolver_name
                && handler_names.contains(name)
            {
                debug!(
                    op = %key.canonical(),
                    file = %path,
                    function = %name,
                    "graphql resolver claim names an HTTP endpoint handler from the \
                     same file's analysis; dropping the claim (borrow witness)"
                );
                continue;
            }
            // The FunctionReturn path needs BOTH a real resolver name AND a
            // usable line: `collect_graphql_producer_infer_requests` skips any
            // producer missing either, so taking this branch with a dead
            // locator would leave the producer with no type request of any
            // kind. The LLM line is a 1-based source line; clamp non-positive
            // values to None rather than wrapping into a bogus u32 (the infer
            // request's line_number is u32, and a 0/negative line can't anchor
            // a fn), and treat a clamped-out line exactly like a missing
            // resolver: fall through to the backing-type fallback.
            let resolver_line = resolver_name
                .and(llm_op.resolver_line)
                .and_then(|line| u32::try_from(line).ok())
                .filter(|&line| line > 0);
            if let Some(line) = resolver_line {
                let name = resolver_name.unwrap_or_default().to_string();
                match resolver_claims.get(&idx) {
                    Some((prev_file, prev_name, _))
                        if prev_file != path.as_str() || prev_name != &name =>
                    {
                        // Guard 2: two files (or two ops) disagree on where the
                        // resolver lives — ambiguous, fail closed below.
                        resolver_conflicts.insert(idx);
                        debug!(
                            op = %key.canonical(),
                            first = %format!("{prev_file}:{prev_name}"),
                            second = %format!("{path}:{name}"),
                            "conflicting graphql resolver claims"
                        );
                    }
                    Some(_) => {} // Duplicate of the accepted claim.
                    None => {
                        resolver_claims.insert(idx, (path.clone(), name, line));
                    }
                }
            } else if let Some(symbol) = llm_op.backing_type_symbol.clone() {
                // No resolver function: the co-located backing type the LLM
                // located (#248). Keyed on the dedicated `backing_type_symbol`
                // (never `primary_type_symbol`, which describes a resolver's
                // return type) so this can only fire for a genuinely
                // resolver-less field.
                match backing_claims.get(&idx) {
                    Some((prev_file, prev_symbol, _))
                        if prev_file != path.as_str() || prev_symbol != &symbol =>
                    {
                        backing_conflicts.insert(idx);
                        debug!(
                            op = %key.canonical(),
                            first = %format!("{prev_file}:{prev_symbol}"),
                            second = %format!("{path}:{symbol}"),
                            "conflicting graphql backing-type claims"
                        );
                    }
                    Some(_) => {}
                    None => {
                        backing_claims.insert(
                            idx,
                            (path.clone(), symbol, llm_op.backing_type_source.clone()),
                        );
                    }
                }
            }
        }
    }

    for (idx, producer) in graphql.producers.iter_mut().enumerate() {
        if resolver_conflicts.contains(&idx) {
            debug!(
                op = %producer.key.canonical(),
                "conflicting resolver locations; leaving the producer unlocated (fail closed)"
            );
            continue;
        }
        if let Some((file, _name, line)) = resolver_claims.get(&idx) {
            // Resolver located: its concrete return type carries the wrappers
            // (Promise / ApiResponse envelope / async-iterator) the bare
            // SDL-backed type can't, so the FunctionReturn path wins — and a
            // backing-type claim for the same key (from any file) never
            // applies alongside it.
            producer.resolver_file = Some(PathBuf::from(file));
            producer.resolver_line = Some(*line);
            continue;
        }
        if backing_conflicts.contains(&idx) {
            debug!(
                op = %producer.key.canonical(),
                "conflicting backing-type claims; leaving the producer unlocated (fail closed)"
            );
            continue;
        }
        if let Some((file, symbol, source)) = backing_claims.get(&idx) {
            // The sidecar bundles + structurally expands the backing type and
            // wraps it in the SDL list depth.
            producer.resolver_file = Some(PathBuf::from(file));
            producer.response_type_symbol = Some(symbol.clone());
            producer.response_type_source = source.clone();
        }
    }
}

/// Fold the file-analyzer's `graphql_consumer_locates` onto document consumers
/// with no deterministic anchor (#268 — the consumer mirror of
/// `merge_graphql_resolver_locations`'s #248 producer backing-type fallback).
///
/// KEYING IS THE LOAD-BEARING DIFFERENCE from the producer merge: a producer's
/// canonical `OperationKey` alone is enough to join on (a schema field has at
/// most one root producer, service-wide). A consumer field has no such
/// uniqueness — the SAME field can be consumed from N different files in a
/// fan-in (a `query order` document duplicated across `web-frontend` and
/// `admin-dashboard`, say), each potentially binding its own local result
/// type. Joining on the canonical key alone would collide every file's locate
/// entry onto whichever consumer op happened to occupy that key first. So this
/// joins on the triple `(file_path, kind, field)`: each file's located type is
/// scoped strictly to its own consumer op. The file is the one the model read,
/// so this runs once the rows at the calls that execute a document are placed
/// ([`settle_graphql_documents`], carrick#1728).
///
/// ISOLATION GUARD: an op that already carries `payload_type_symbol` (the
/// deterministic `TaggedTplVisitor::capture_request_call` explicit-generic
/// anchor) or `declared_result_type` (the field's type as the executed
/// document's declaration states it, carrick#1761) is left untouched. The file-analyzer is instructed not to emit a
/// `graphql_consumer_locates` entry for an already-anchored op, but a
/// stray/hallucinated entry must never be allowed to override it regardless —
/// mirrors 186cb27's resolver-first gate on the producer side.
fn merge_graphql_consumer_locations(
    graphql: &mut crate::graphql::GraphqlExtraction,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
) {
    // (file_path, canonical_key) -> index. A consumer op's identity for this
    // join is its file AND its operation key together — never the key alone.
    // Both sides are read repo-relative: the op carries the path the scan
    // discovered, and the incremental path's answers are keyed repo-relative
    // (carrick#1725).
    let mut by_file_key: HashMap<(String, String), usize> = HashMap::new();
    for (idx, op) in graphql.consumers.iter().enumerate() {
        by_file_key.insert(
            (
                repo_relative(&op.file_path.to_string_lossy(), repo_path),
                op.key.canonical(),
            ),
            idx,
        );
    }

    for (path, result) in file_results {
        for locate in &result.graphql_consumer_locates {
            let key = OperationKey::graphql(locate.kind, locate.field.clone());
            let file = repo_relative(path, repo_path);
            let Some(&idx) = by_file_key.get(&(file, key.canonical())) else {
                debug!(
                    op = %key.canonical(),
                    file = %path,
                    "graphql_consumer_locate has no matching consumer op in this file; ignoring"
                );
                continue;
            };
            let consumer = &mut graphql.consumers[idx];
            if consumer.payload_type_symbol.is_some() || consumer.declared_result_type.is_some() {
                // Isolation guard: an explicit call-site generic, or the
                // result type the executed document's declaration states
                // (carrick#1761), already anchored this op — never let a
                // located type override it.
                continue;
            }
            consumer.consumer_located_type_symbol = Some(locate.result_type_symbol.clone());
            consumer.consumer_located_type_source = locate.result_type_source.clone();
        }
    }
}

/// Emit type-manifest entries for the deterministically-extracted GraphQL and
/// Socket.IO operations (#245 Phase 1). Without this, only the HTTP mount-graph
/// produces manifest entries, so every non-HTTP op reported `type_state=(none)`
/// with no `type_alias`/anchor.
///
/// Each op gets a single Response-kind entry keyed by its real `OperationKey`
/// (so `OperationKey::canonical()` joins it in the cloud index and eval
/// projection). Listeners / SDL producers are `Producer`; emitters / document
/// consumers are `Consumer`. Every op gets a Response-kind entry. A Request
/// entry is emitted only where the request is stated: an SDL root field that
/// declares arguments ([`add_graphql_request_entry`], carrick#1158). Anywhere
/// else a Request alias would never resolve and would drag a second `Unknown`
/// entry into the manifest.
///
/// Socket entries carry `primary_type_symbol` directly (the payload type the
/// extractor captured), which the sidecar then resolves through the existing
/// SymbolRequest path. GraphQL SDL producers carry their deterministic anchor
/// too (#248): the root field's SDL type expression (`Order`, `[Order!]!`),
/// the only anchor available without a framework-specific SDL-field → TS-resolver
/// mapping. GraphQL document consumers have no SDL type, so their anchor is
/// the call-site-bound `payload_type_symbol`, falling back to the
/// file-analyzer-located `consumer_located_type_symbol` (#268) when the
/// deterministic pass found no explicit call-site generic.
fn append_protocol_manifest_entries(
    entries: &mut Vec<TypeManifestEntry>,
    extractions: &ProtocolExtractions,
    library: &LibrarySiteIndex,
) {
    let sockets = library.typed_sockets(&extractions.sockets);
    for op in &extractions.graphql.producers {
        add_protocol_manifest_entry(
            entries,
            &op.key,
            ManifestRole::Producer,
            &op.file_path.to_string_lossy(),
            op.line,
            op.primary_type_symbol.clone(),
            None,
        );
        if let Some(arguments) = &op.arguments {
            add_graphql_request_entry(
                entries,
                op,
                arguments,
                &extractions.graphql.input_declarations,
            );
        }
    }
    for op in &extractions.graphql.consumers {
        add_protocol_manifest_entry(
            entries,
            &op.key,
            ManifestRole::Consumer,
            &op.file_path.to_string_lossy(),
            op.line,
            // The consumer's real anchor is the bound TS result type captured at
            // the `request<T>(DOC)` call site (#248 consumer side), not the
            // SDL-derived `primary_type_symbol` (always `None` for documents).
            // When the deterministic pass found no explicit call-site generic,
            // fall back to the file-analyzer-located co-located type (#268) —
            // the engine merge's isolation guard already guarantees an op never
            // carries both, so this is a plain either/or, not a priority
            // decision made here. A located type that is the result of the
            // row's whole operation names the wrapper, not the row's type: the
            // row reads its field's property instead (carrick#1760).
            op.payload_type_symbol
                .clone()
                .or_else(|| match op.located_field_type {
                    Some(_) => None,
                    None => op.consumer_located_type_symbol.clone(),
                }),
            // Fan-in consumers (multiple repos reading the same field) carry the
            // same latent alias-collision risk the pub/sub publisher path fixes,
            // but neither corpus exercises it today; deferred to #291.
            None,
        );
    }
    for op in &sockets.listeners {
        add_protocol_manifest_entry(
            entries,
            &op.key,
            ManifestRole::Producer,
            &op.file_path.to_string_lossy(),
            op.line,
            op.payload_type_symbol.clone(),
            None,
        );
    }
    for op in &sockets.emitters {
        add_protocol_manifest_entry(
            entries,
            &op.key,
            ManifestRole::Consumer,
            &op.file_path.to_string_lossy(),
            op.line,
            op.payload_type_symbol.clone(),
            // Same deferred fan-in caveat as the graphql consumer above (#291).
            None,
        );
    }
}

/// The Request-kind entry for an SDL root field that declares arguments
/// (carrick#1158).
///
/// The definition is the schema's own statement of the request, so it is
/// written here rather than asked of the type sidecar: the field's argument
/// list and the `input`, `enum` and `scalar` declarations it reaches, printed as
/// SDL ([`crate::graphql::request_definition`]). The entry is `Explicit` because
/// the schema declares it. No capture anchor is emitted for it, so the capture
/// never answers for its alias and the definition stands. GraphQL consumers
/// carry no Request entry, so no compatibility pair forms on it.
///
/// `primary_type_symbol` is the one `input` object the arguments name
/// directly, when there is exactly one, which is the name an agent asks
/// `get_type_definition` for (`CreateInvoiceInput`).
fn add_graphql_request_entry(
    entries: &mut Vec<TypeManifestEntry>,
    op: &crate::graphql::GraphqlOp,
    arguments: &crate::graphql::SdlArguments,
    declarations: &std::collections::BTreeMap<String, crate::graphql::SdlInputDeclaration>,
) {
    let (definition, symbol) = crate::graphql::request_definition(arguments, declarations);
    let role = ManifestRole::Producer;
    let type_kind = ManifestTypeKind::Request;
    let file_path = op.file_path.to_string_lossy().to_string();
    entries.push(TypeManifestEntry {
        key: op.key.clone(),
        role,
        type_kind,
        type_alias: crate::type_manifest::build_manifest_type_alias_with_site_id(
            &op.key, role, type_kind, None,
        ),
        file_path: file_path.clone(),
        line_number: op.line,
        is_explicit: true,
        type_state: ManifestTypeState::Explicit,
        evidence: crate::cloud_storage::TypeEvidence {
            file_path,
            span_start: None,
            span_end: None,
            line_number: op.line,
            infer_kind: infer_kind_for_manifest(role, type_kind),
            is_explicit: true,
            type_state: ManifestTypeState::Explicit,
        },
        resolved_definition: Some(definition.clone()),
        expanded_definition: Some(definition),
        primary_type_symbol: symbol,
        defined_in: None,
        any_provenance: Vec::new(),
        unwidened_definition: None,
        v1_state_before_demotion: None,
    });
}

/// Add a single Response-kind manifest entry for a non-HTTP operation. Shared
/// by `append_protocol_manifest_entries`; the HTTP path uses
/// `add_manifest_pair` instead (it emits both Request and Response and dispatches
/// on the HTTP method).
///
/// `primary_type_symbol` is threaded straight onto the entry at creation — the
/// op carries its anchor deterministically, unlike HTTP where it is stamped
/// later from the LLM result. The `type_alias` MUST be computed with the same
/// `build_manifest_type_alias_with_site_id(key, role, Response, call_id)` the
/// SymbolRequest side uses — same key, same role, same kind, AND the same
/// `call_id` (see the `call_id` param) — or the enrich-join silently fails to
/// flip `Unknown` → resolved.
fn add_protocol_manifest_entry(
    entries: &mut Vec<TypeManifestEntry>,
    key: &OperationKey,
    role: ManifestRole,
    file_path: &str,
    line_number: u32,
    primary_type_symbol: Option<String>,
    // Per-call-site disambiguator. `None` keeps the plain key-only alias (one
    // definition per key per repo — correct for producers/endpoints). `Some`
    // appends a `_Call<id>` suffix so multiple consumers of the same key in one
    // bundle (fan-in) don't collide on a single alias; see the pub/sub publisher
    // path in `append_pubsub_manifest_entries`. Must equal the `call_id` the
    // SymbolRequest side computes for the same op, or the resolution join breaks.
    call_id: Option<&str>,
) {
    let type_kind = ManifestTypeKind::Response;
    let type_alias =
        crate::type_manifest::build_manifest_type_alias_with_site_id(key, role, type_kind, call_id);
    let infer_kind = infer_kind_for_manifest(role, type_kind);
    let evidence = crate::cloud_storage::TypeEvidence {
        file_path: file_path.to_string(),
        span_start: None,
        span_end: None,
        line_number,
        infer_kind,
        is_explicit: false,
        type_state: ManifestTypeState::Unknown,
    };
    entries.push(TypeManifestEntry {
        key: key.clone(),
        role,
        type_kind,
        type_alias,
        file_path: file_path.to_string(),
        line_number,
        is_explicit: false,
        type_state: ManifestTypeState::Unknown,
        evidence,
        resolved_definition: None,
        expanded_definition: None,
        primary_type_symbol,
        defined_in: None,
        any_provenance: Vec::new(),
        unwidened_definition: None,
        v1_state_before_demotion: None,
    });
}

/// Reduce a source path to its repo-root-relative form, with forward slashes.
/// Absolute paths under the root are stripped; anything else (a path outside
/// the root, an already-relative path) passes through unchanged, which is what
/// makes the function idempotent and safe to run over a payload whose paths
/// are already relative.
///
/// This is the one place the repo root is stripped from a string path. Callers:
/// the cloud-projection pass below, and the v2 capture wire (the sidecar joins
/// `source_file` back onto `repo_root`).
pub(crate) fn repo_relative(file_path: &str, repo_root: &str) -> String {
    let root = repo_root.trim_end_matches('/');
    let stripped = if root.is_empty() || root == "." {
        file_path
    } else {
        file_path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(file_path)
    };
    let stripped = stripped.strip_prefix("./").unwrap_or(stripped);
    if cfg!(windows) {
        stripped.replace('\\', "/")
    } else {
        stripped.to_string()
    }
}

/// `repo_relative` for a `PathBuf` field.
fn relativize_path_buf(path: &mut PathBuf, repo_root: &str) {
    let relative = repo_relative(&path.to_string_lossy(), repo_root);
    if relative != path.to_string_lossy() {
        *path = PathBuf::from(relative);
    }
}

/// Make every path in a cloud-bound payload repo-relative, once, at the
/// boundary where analysis results stop being local coordinates and become
/// index data.
///
/// The scan runs against a canonicalized absolute repo root (in CI the runner
/// checkout, e.g. `/home/runner/work/<dir>/<repo>`), and that root ends up
/// stamped on every location the analysis produces. Whether it SURVIVES to the
/// upload used to depend on which branch ran: the incremental branch normalizes
/// `file_results` keys before rebuilding the mount graph, so every
/// `file_location` derived from a key came out relative, while the full branch
/// normalized only the cached copy — after the graph was already projected — so
/// the same fields came out absolute. Same repo, different day, different
/// answer; and a consumer joining the path back to the repo (GitHub deep links,
/// MCP tools fetching `repos/{owner}/{repo}/contents/{file_path}`) got a path
/// that resolves to nothing.
///
/// Run this at the end of BOTH branches instead. It is idempotent, so the
/// incremental branch (already relative) is unaffected, and it is a structural
/// strip of the real root — never a match on `/home/runner/`, which would only
/// cover GitHub-hosted runners.
///
/// Printed type text is a second kind of path carrier: the compiler names an
/// out-of-scope module by its absolute path, which can sit under the checkout
/// root, a package store or a runtime cache in the scanning account's home
/// directory (carrick#1160). Every such string goes through `scrub`, which
/// turns a package path into `<name>@<version>` and strips the root and home.
///
/// Deliberately NOT rewritten: the declaration files in `capture_stub.files`.
/// They are re-materialized into the synthetic type-check workspace verbatim,
/// and rewriting module specifiers inside them would change what tsc resolves.
/// The stub's `carrick-manifest.json` is not compiled — it is the capture's
/// per-alias record, whose detail sentences name the modules that failed — so
/// it is scrubbed like any other served text. `mounts`, `apps` and
/// `imported_handlers` carry no paths.
fn relativize_cloud_paths(
    cloud_data: &mut CloudRepoData,
    repo_path: &str,
    scrub: &served_paths::PathScrub,
) {
    let prefix = format!("{}/", repo_path.trim_end_matches('/'));

    // Endpoints and calls: the op's own source location, plus the source file
    // of any attached type reference.
    for op in cloud_data
        .endpoints
        .iter_mut()
        .chain(cloud_data.calls.iter_mut())
    {
        relativize_path_buf(&mut op.file_path, repo_path);
        for type_ref in [op.request_type.as_mut(), op.response_type.as_mut()]
            .into_iter()
            .flatten()
        {
            relativize_path_buf(&mut type_ref.file_path, repo_path);
        }
    }

    // Mount graph: the source of truth every projection above is derived from,
    // and itself uploaded for cross-repo matching.
    if let Some(graph) = cloud_data.mount_graph.as_mut() {
        for node in graph.nodes.values_mut() {
            node.file_location = repo_relative(&node.file_location, repo_path);
            if let Some(site) = node.creation_site.as_mut() {
                *site = repo_relative(site, repo_path);
            }
        }
        for endpoint in &mut graph.endpoints {
            endpoint.file_location = repo_relative(&endpoint.file_location, repo_path);
        }
        for call in &mut graph.data_calls {
            call.file_location = repo_relative(&call.file_location, repo_path);
            // The request a wrapper call reaches (carrick#1402) is a location
            // in the same `"<file>:<line>"` form, written by a join that reads
            // the scan's own paths, so it is relativized with the row's own.
            if let Some(site) = call.reaches_request.as_mut() {
                *site = repo_relative(site, repo_path);
            }
        }
    }

    // Type manifest: the entry location and the evidence location are separate
    // fields and both are read as repo coordinates.
    if let Some(entries) = cloud_data.type_manifest.as_mut() {
        for entry in entries {
            entry.file_path = repo_relative(&entry.file_path, repo_path);
            entry.evidence.file_path = repo_relative(&entry.evidence.file_path, repo_path);
            // Both definition strings are printed TypeScript, so they carry
            // the same `import("/abs/path")` leak the bundle does — and
            // `expanded_definition` is what the PR comment prints as the type
            // label on a mismatch row, so an absolute root here is rendered,
            // not just stored.
            for definition in [
                entry.resolved_definition.as_mut(),
                entry.expanded_definition.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                scrub.in_place(definition);
            }
            for finding in &mut entry.any_provenance {
                if let Some(detail) = finding.detail.as_mut() {
                    scrub.in_place(detail);
                }
            }
        }
    }

    // Function definitions are already stripped on both branches (see
    // `relativize_function_definition_paths`); re-running is a no-op and keeps
    // the invariant true for any future construction path that forgets to.
    relativize_function_definition_paths(&mut cloud_data.function_definitions, repo_path, scrub);

    // The compiler leaks the absolute root into the bundle as
    // `import("/abs/path/x")`, the same way it does into signatures.
    if let Some(bundled) = cloud_data.bundled_types.as_mut() {
        scrub.in_place(bundled);
    }

    // The capture's per-alias record: served text, not compiled (see above).
    if let Some(record) = cloud_data
        .capture_stub
        .as_mut()
        .and_then(|stub| stub.files.get_mut(CAPTURE_MANIFEST_FILE))
    {
        scrub.in_place(record);
    }

    // Package manifest locations. `merged_dependencies` holds its own copy of
    // the source path (`Packages::resolve_dependencies` clones it per entry),
    // so both have to be walked; `package_json` is a serialization of the same
    // struct, so re-serialize rather than leave the two disagreeing.
    if let Some(packages) = cloud_data.packages.as_mut() {
        for path in &mut packages.source_paths {
            relativize_path_buf(path, repo_path);
        }
        for info in packages.merged_dependencies.values_mut() {
            relativize_path_buf(&mut info.source_path, repo_path);
        }
        if let Ok(json) = serde_json::to_string(packages) {
            cloud_data.package_json = Some(json);
        }
    }

    // Cache keys: normalized on both branches already, re-applied here so the
    // invariant is enforced in one place. Gated on an actual offender —
    // `normalize_file_results_keys` clones every result, which is the multi-MB
    // bulk of the payload.
    if cloud_data.file_results.as_ref().is_some_and(|results| {
        results
            .keys()
            .any(|key| key.starts_with(&prefix) || key.starts_with("./"))
    }) && let Some(file_results) = cloud_data.file_results.take()
    {
        cloud_data.file_results = Some(normalize_file_results_keys(&file_results, repo_path));
    }

    // Outbound-call candidates are relativized at construction; keep them in
    // the sweep so the invariant does not depend on that staying true.
    if let Some(candidates) = cloud_data.external_call_candidates.as_mut() {
        for candidate in candidates {
            candidate.file = repo_relative(&candidate.file, repo_path);
        }
    }

    // Published SDK members, same story: the walker strips the root it was
    // given, and the sweep holds the invariant if it ever cannot.
    if let Some(members) = cloud_data.sdk_surface.as_mut() {
        for member in members {
            member.file = repo_relative(&member.file, repo_path);
            for span in member.delegates.iter_mut() {
                span.file = repo_relative(&span.file, repo_path);
            }
        }
    }
}

/// The capture's per-alias record inside the stub file map.
const CAPTURE_MANIFEST_FILE: &str = "carrick-manifest.json";

/// Repo-relative paths for cloud-bound function definitions. The scan runs
/// against a canonicalized absolute repo root (in CI the runner checkout,
/// e.g. `/home/runner/work/<dir>/<repo>`), and the extractor stamps that
/// absolute path onto every definition — plus the compiler leaks it into
/// signatures via `import("/abs/path/x")` type references. Uploading those
/// verbatim breaks every consumer that joins the path back to the repo
/// (GitHub deep links, MCP tools telling agents to fetch
/// `repos/{owner}/{repo}/contents/{file_path}`). Strip the root at the
/// cloud-projection boundary only — internal passes (sidecar type
/// resolution, git-diff comparisons) still operate on absolute paths.
///
/// Every printed type on the row goes through `scrub`: the composed
/// `signature`, and the `return_type` and parameter `type_string`s it was
/// composed from, which are served on their own (carrick#1160).
fn relativize_function_definition_paths(
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    repo_path: &str,
    scrub: &served_paths::PathScrub,
) {
    let root = std::path::Path::new(repo_path);
    let prefix = format!("{}/", repo_path.trim_end_matches('/'));
    for def in function_definitions.values_mut() {
        if let Ok(stripped) = def.file_path.strip_prefix(root) {
            def.file_path = stripped.to_path_buf();
        }
        for call in &mut def.calls {
            if let Some(stripped) = call.file_path.strip_prefix(&prefix) {
                call.file_path = stripped.to_string();
            }
        }
        for printed in def
            .arguments
            .iter_mut()
            .filter_map(|argument| argument.type_string.as_mut())
            .chain(def.return_type.as_mut())
            .chain(def.signature.as_mut())
        {
            scrub.in_place(printed);
        }
    }
}

/// Attach the service's outbound-call candidates to the payload (carrick#510).
///
/// Two sources, one list: the deterministic SDK scan over the service's files,
/// and the declared-external HTTP calls projected out of the mount graph the
/// payload already carries. Reading the graph back off `cloud_data` rather than
/// taking it as an argument keeps the rows and the uploaded graph provably the
/// same set of calls.
///
/// Called on both analysis paths and deliberately outside the incremental
/// cache: the SDK scan is pure AST over the tree on disk, and `files` is always
/// the full service file list on both paths, so a cached run and a full run
/// produce the same rows.
///
/// The dependency set the SDK scan resolves against comes from every manifest
/// in the repo rather than from `packages`, which is scoped to this service's
/// own `package.json` — in a monorepo the manifest declaring a vendor client is
/// usually the shared package holding the wrapper, not the service shipping the
/// call.
///
/// `None` rather than an empty vector when there is nothing to report, so the
/// cloud can tell "no candidates" apart from "scanned before this channel
/// existed".
fn attach_external_call_candidates(
    cloud_data: &mut CloudRepoData,
    repo_path: &str,
    files: &[PathBuf],
    config: &Config,
    workspace: &mut crate::external_call_candidates::WorkspaceScan,
) {
    let repo_root = std::path::Path::new(repo_path);
    let started = Instant::now();
    let sdk_rows = workspace.rows_for_service(files, repo_root);
    debug!(
        "External call candidates: workspace pass in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let normalizer = UrlNormalizer::new(config);
    let http_rows = cloud_data
        .mount_graph
        .as_ref()
        .map(|graph| {
            crate::external_call_candidates::from_data_calls(
                &graph.data_calls,
                repo_root,
                config,
                &normalizer,
            )
        })
        .unwrap_or_default();
    debug!(
        "External call candidates: {} sdk + {} http row(s) for {}",
        sdk_rows.len(),
        http_rows.len(),
        cloud_data.repo_name
    );
    let candidates = crate::external_call_candidates::merge(sdk_rows, http_rows);
    if !candidates.is_empty() {
        cloud_data.external_call_candidates = Some(candidates);
    }
}

/// Compute the callable surface this service publishes as an npm package and
/// attach it to the payload (carrick#466).
///
/// Deterministic and AST-only, and empty for the overwhelming majority of
/// services — a repo that publishes no client library resolves an entry module
/// whose exports reach no class or object literal, and contributes nothing.
/// Absent rather than empty in that case, the same convention
/// `external_call_candidates` follows: a peer with `None` predates the
/// channel, and the join says so.
fn attach_sdk_surface(cloud_data: &mut CloudRepoData, repo_path: &str, config: &Config) {
    let repo_root = std::path::Path::new(repo_path);
    let started = Instant::now();
    let members = crate::sdk_surface::scan(repo_root, &service_scan_root(repo_path, config));
    debug!(
        "SDK surface: entry walk in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    debug!(
        "SDK surface: {} member(s) for {}",
        members.len(),
        cloud_data.repo_name
    );
    if !members.is_empty() {
        cloud_data.sdk_surface = Some(members);
    }
}

/// Build `CloudRepoData` from a mount graph (used by the incremental path).
fn build_cloud_data_from_mount_graph(
    repo_name: &str,
    repo_path: &str,
    mount_graph: &MountGraph,
    config: &Config,
    packages: &Packages,
    function_definitions: HashMap<String, FunctionDefinition>,
) -> CloudRepoData {
    let mut function_definitions = function_definitions;
    relativize_function_definition_paths(
        &mut function_definitions,
        repo_path,
        &served_paths::PathScrub::for_scan(repo_path),
    );
    let config_json = serde_json::to_string(config).ok();
    let service_name = config_json.as_ref().and_then(|json| {
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|v| {
                v.get("serviceName")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
    });

    // Project endpoints + consumer calls through the shared helper so the
    // consumer key is the pre-computed `canonical_path` (identical to the
    // manifest join key).
    let (endpoints, calls) = mount_graph_to_api_details(mount_graph);

    let mounts: Vec<crate::visitor::Mount> = mount_graph
        .get_mounts()
        .iter()
        .map(|mount| crate::visitor::Mount {
            parent: crate::visitor::OwnerType::App(mount.parent.clone()),
            child: crate::visitor::OwnerType::Router(mount.child.clone()),
            prefix: mount.path_prefix.clone(),
        })
        .collect();

    debug!(
        "CloudRepoData from incremental: {} endpoints, {} calls, {} mounts",
        endpoints.len(),
        calls.len(),
        mounts.len()
    );

    CloudRepoData {
        repo_name: repo_name.to_string(),
        service_name,
        endpoints,
        calls,
        mounts,
        apps: HashMap::new(),
        imported_handlers: vec![],
        function_definitions,
        config_json,
        package_json: serde_json::to_string(packages).ok(),
        packages: Some(packages.clone()),
        last_updated: chrono::Utc::now(),
        commit_hash: get_current_commit_hash(repo_path),
        dirty: None,
        mount_graph: Some(mount_graph.clone()),
        bundled_types: None,
        type_manifest: None,
        file_results: None,
        cached_detection: None,
        cached_guidance: None,
        cached_extraction_config: None,
        package_json_hash: None,
        cache_version: None,
        type_extraction_status: None,
        types_degraded: None,
        compat_verdicts: None,
        capture_stub: None,
        external_call_candidates: None,
        sdk_surface: None,
        sdk_edges: None,
        sdk_unresolved: None,
        // Stamp the release that produced this blob so the cloud can tell
        // "same commit, same scanner" (skip) from "same commit, newer
        // scanner" (re-index).
        scanner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        // And the build, which names the code even between releases (#1739).
        scanner_build: crate::cloud_storage::ScannerBuild::current(),
        boundary: None,
        dispatch_tables: None,
    }
}

/// Re-scope the already-spawned sidecar to a single service's project so type
/// extraction uses that service's directory and tsconfig instead of the whole
/// repo. No-op for a whole-repo service (no `directory` and no `tsconfig`),
/// which keeps the single-service path on the warm init done in `main`.
///
/// Re-init rebuilds the sidecar's ts-morph project; for a large monorepo this
/// runs once per scoped service. A future optimization could load all service
/// projects in a single init via the sidecar's monorepo builder.
/// The root that file-based route derivation strips before matching a
/// convention's root globs. Conventions declare app-root globs like `app` /
/// `src/app`, which are SERVICE-relative: in a monorepo the app lives at e.g.
/// `apps/web/app/**`, so stripping only the repo root would leave a path no
/// glob matches and silently derive zero routes for every declared service.
fn service_scan_root(repo_path: &str, service: &Config) -> std::path::PathBuf {
    match &service.directory {
        Some(dir) => Path::new(repo_path).join(dir),
        None => Path::new(repo_path).to_path_buf(),
    }
}

fn scope_sidecar_to_service(sidecar: Option<&TypeSidecar>, repo_path: &str, service: &Config) {
    let Some(sidecar) = sidecar else { return };

    // Match main()'s absolute-path init so the sidecar resolves files the same way.
    let canonical =
        std::fs::canonicalize(repo_path).unwrap_or_else(|_| std::path::PathBuf::from(repo_path));
    let service_root = match &service.directory {
        Some(dir) => canonical.join(dir),
        None => canonical,
    };
    let label = service.service_name.as_deref().unwrap_or("(root)");

    debug!(
        "Re-initializing sidecar for service '{}' at {}",
        label,
        service_root.display()
    );
    sidecar.start_init(&service_root, service.tsconfig.as_deref());
    if let Err(e) = sidecar.wait_ready(crate::services::type_sidecar::ready_budget()) {
        warn!(
            "Sidecar re-init for service '{}' failed: {} — type extraction may be skipped \
             for this service",
            label, e
        );
        crate::scan_health::record_types_unavailable(label, &e.to_string());
    }
}

/// One finding per service whose type extraction failed, so the loss is
/// reported rather than logged (carrick#535). Services that resolved types
/// normally — including ones with per-symbol failures, which are not a
/// whole-service degradation — contribute nothing.
fn degraded_type_findings(services: &[CloudRepoData]) -> Vec<crate::findings::Finding> {
    services
        .iter()
        .filter_map(|data| {
            let degraded = data.types_degraded.as_ref()?;
            let service = data
                .service_name
                .clone()
                .unwrap_or_else(|| data.repo_name.clone());
            Some(crate::findings::Finding::degraded_types(
                service,
                degraded.stage.clone(),
                &degraded.detail,
            ))
        })
        .collect()
}

/// Resolve types via sidecar if available (shared logic for full and incremental paths).
#[allow(clippy::too_many_arguments)]
/// Resolve types through the sidecar and run the v2 capture for this
/// service. Returns the on-disk capture stub dir when capture succeeded (the
/// definitions re-point reads from it; the caller owns cleanup).
/// The module resolver for one service: every alias its own config declares,
/// and the config `carrick.json` names for it when it names one
/// (carrick#1416).
///
/// Built once per service scan and shared by everything that turns an import
/// specifier into a file for the sidecar. It reads the repo's config files, so
/// it costs a walk — but every service with a typed endpoint needs it, and the
/// alternative is the second, weaker resolver this replaced.
fn service_module_index(
    repo_path: &str,
    config: &Config,
) -> crate::workspace_resolver::WorkspaceIndex {
    let service_tsconfig = config.alias_tsconfig();
    crate::workspace_resolver::WorkspaceIndex::build_with_aliases(
        Path::new(repo_path),
        service_tsconfig
            .as_ref()
            .map(|(directory, tsconfig)| (directory.as_path(), tsconfig.as_path())),
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_types_if_available(
    sidecar: Option<&TypeSidecar>,
    file_orchestrator: &FileOrchestrator,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
    extraction_config: Option<&crate::services::type_sidecar::ExtractionConfig>,
    mount_graph: &MountGraph,
    config: &Config,
    modules: &crate::workspace_resolver::WorkspaceIndex,
    extra_explicit: &[crate::services::type_sidecar::SymbolRequest],
    extra_infer: &[crate::services::type_sidecar::InferRequestItem],
    cloud_data: &mut CloudRepoData,
) -> Option<PathBuf> {
    let Some(sidecar) = sidecar else {
        // The reason the run recorded at spawn, not a list of everything that
        // could have gone wrong: this string is what the boundary line reads
        // out to whoever is looking at a service with no types.
        let detail =
            match crate::scan_health::types_unavailable_reason(crate::scan_health::WHOLE_SCAN) {
                Some(reason) => format!("sidecar unavailable: {}", reason),
                None => "sidecar unavailable (not found, failed to start, or failed to \
                     initialize)"
                    .to_string(),
            };
        cloud_data.type_extraction_status = Some(format!("type extraction skipped: {}", detail));
        cloud_data.types_degraded = Some(TypeDegradation {
            stage: "spawn".to_string(),
            detail,
        });
        return None;
    };

    debug!("Starting sidecar type resolution");
    match sidecar.wait_ready(crate::services::type_sidecar::ready_budget()) {
        Ok(()) => {
            match file_orchestrator.resolve_types_with_sidecar(
                sidecar,
                file_results,
                repo_path,
                extraction_config,
                mount_graph,
                config,
                modules,
                extra_explicit,
                extra_infer,
            ) {
                Ok(type_resolution) => {
                    debug!(
                        "Type resolution: {} explicit, {} inferred, {} failures",
                        type_resolution.explicit_manifest.len(),
                        type_resolution.inferred_types.len(),
                        type_resolution.symbol_failures.len()
                    );
                    cloud_data.bundled_types = type_resolution.dts_content.clone();
                    if let Some(ref mut manifest) = cloud_data.type_manifest {
                        // Before enrichment, whose inference-anchor fill reads
                        // only rows left without a symbol.
                        restamp_arbitrated_anchors(
                            manifest,
                            &type_resolution.anchor_changes,
                            repo_path,
                        );
                        anchor_stated_body_roots(
                            manifest,
                            &type_resolution.inferred_types,
                            repo_path,
                        );
                        enrich_manifest_with_type_resolution(
                            manifest,
                            &type_resolution,
                            type_resolution.dts_content.as_deref(),
                        );
                    }
                    // Per-symbol failures are logged in FileOrchestrator at the
                    // resolution call site (with capped warn + spillover to debug).

                    // A missing extraction config means machinery wrappers stay
                    // wrapped: types still resolve, but an `AxiosResponse<T>`
                    // surfaces as itself. That is a failure only when the rules
                    // were asked for — they are generated by the model, so a
                    // pass that asked for no model never had any and nothing
                    // failed. Recording it as a failure there put the sentence
                    // on every free-pass blob, and it became a false warning
                    // the moment the boundary printed it (carrick#997 item 3).
                    // An empty rule set arrives as `Some(config)` with zero
                    // rules, so it is not this branch.
                    if extraction_config.is_none() && !crate::local_mode::no_model() {
                        cloud_data.type_extraction_status = Some(
                            "machinery unwrapping disabled this run: extraction-config \
                             generation failed; wrapper types (e.g. AxiosResponse<T>) may \
                             surface in the type manifest"
                                .to_string(),
                        );
                    }

                    // v2 capture ("tsc as the serializer"): derive anchors
                    // from the SAME collected requests the bundle path used —
                    // collect_type_requests is deterministic over the same
                    // inputs, so the aliases are byte-identical to the
                    // manifest join keys. The #413 borrow-witness demotion and
                    // the inference-resolved array depths are applied exactly
                    // as resolve_all_types does (#306).
                    crate::phase_timing::mark(crate::phase_timing::Phase::Types);
                    let captured = run_capture_for_service(
                        sidecar,
                        file_orchestrator,
                        file_results,
                        repo_path,
                        mount_graph,
                        config,
                        modules,
                        extra_explicit,
                        extra_infer,
                        &type_resolution,
                        cloud_data,
                    );
                    crate::phase_timing::mark(crate::phase_timing::Phase::Capture);
                    captured
                }
                Err(e) => {
                    warn!("Type resolution failed: {}", e);
                    debug!("Continuing without bundled types");
                    cloud_data.type_extraction_status =
                        Some(format!("type resolution failed: {}", e));
                    cloud_data.types_degraded = Some(TypeDegradation {
                        stage: "resolve".to_string(),
                        detail: e.to_string(),
                    });
                    None
                }
            }
        }
        Err(e) => {
            warn!("Sidecar not ready: {}", e);
            debug!("Skipping type resolution");
            cloud_data.type_extraction_status =
                Some(format!("type extraction skipped: sidecar not ready: {}", e));
            cloud_data.types_degraded = Some(TypeDegradation {
                stage: "init".to_string(),
                detail: e.to_string(),
            });
            crate::scan_health::record_types_unavailable(
                config.service_name.as_deref().unwrap_or("(root)"),
                &e.to_string(),
            );
            None
        }
    }
}

/// Derive capture anchors and run `capture_v2` for one service, storing the
/// stub artifact on its `CloudRepoData`. Returns the on-disk stub dir for
/// the definitions re-point (caller cleans it up). Non-fatal: a degraded
/// capture leaves `capture_stub` unset — the service's cross-repo pairs then
/// verdict unverifiable, never silently compatible.
#[allow(clippy::too_many_arguments)]
fn run_capture_for_service(
    sidecar: &TypeSidecar,
    file_orchestrator: &FileOrchestrator,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
    mount_graph: &MountGraph,
    config: &Config,
    modules: &crate::workspace_resolver::WorkspaceIndex,
    extra_explicit: &[crate::services::type_sidecar::SymbolRequest],
    extra_infer: &[crate::services::type_sidecar::InferRequestItem],
    type_resolution: &TypeResolutionResult,
    cloud_data: &mut CloudRepoData,
) -> Option<PathBuf> {
    crate::scan_stage::enter(crate::scan_stage::Stage::TypeCapture);
    let (mut explicit, mut infer, inline_aliases) = file_orchestrator.collect_type_requests(
        file_results,
        repo_path,
        mount_graph,
        config,
        modules,
    );
    explicit.extend_from_slice(extra_explicit);
    infer.extend_from_slice(extra_infer);
    // Mirror resolve_all_types' post-processing exactly: the #413 two-anchor
    // arbitration re-aims witnessed-borrowed pub/sub anchors at the
    // tsc-witnessed payload root BEFORE depths apply, so v2 capture anchors
    // the same symbol the v1 bundle defines the alias from.
    let explicit = crate::services::type_sidecar::demote_witnessed_borrowed_anchors(
        &explicit,
        &type_resolution.inferred_types,
    )
    .requests;
    let explicit = crate::services::type_sidecar::apply_inferred_array_depth(
        &explicit,
        &type_resolution.inferred_types,
    );

    let absolute_repo = Path::new(repo_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(repo_path));
    let capture_root = absolute_repo.join(config.directory.as_deref().unwrap_or("."));
    let capture_root = capture_root.canonicalize().unwrap_or(capture_root);
    let capture_root = capture_root.to_string_lossy();
    // Requests are collected relative to the repository, while capture uses
    // the same service root as init. Absolute paths preserve shared includes.
    let mut explicit = explicit;
    for request in &mut explicit {
        if Path::new(&request.source_file).is_relative() {
            request.source_file = absolute_repo
                .join(&request.source_file)
                .to_string_lossy()
                .into_owned();
        }
    }
    for request in &mut infer {
        if Path::new(&request.file_path).is_relative() {
            request.file_path = absolute_repo
                .join(&request.file_path)
                .to_string_lossy()
                .into_owned();
        }
    }
    // The consumer calls the check may retype with a producer's response
    // (carrick#1491), under the root `scope_sidecar_to_service` initialises
    // this service at, so the check re-uses the program when it is still
    // scoped here.
    let service_id = cloud_data
        .service_name
        .clone()
        .unwrap_or_else(|| cloud_data.repo_name.clone());
    type_compat_v2::record_local_consumer(
        &service_id,
        type_compat_v2::LocalConsumer {
            root: match &config.directory {
                Some(dir) => absolute_repo.join(dir),
                None => absolute_repo.clone(),
            },
            tsconfig: config.tsconfig.clone(),
            calls: type_compat_v2::consumer_call_locators(&infer),
        },
    );
    // Every alias the check phase will import from this service's surface. The
    // manifest is already on `cloud_data` by the time capture runs.
    let manifest_aliases: Vec<String> = cloud_data
        .type_manifest
        .as_ref()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.type_alias.clone())
                .collect()
        })
        .unwrap_or_default();
    let anchors = type_compat_v2::derive_capture_anchors(
        &explicit,
        &infer,
        &inline_aliases,
        &type_resolution.inferred_types,
        &manifest_aliases,
        &capture_root,
    );
    if anchors.is_empty() {
        return None;
    }

    // Literal texts for the last-resort backfill re-anchor: when capture
    // demotes an alias (e.g. its file's declaration emit was skipped), the
    // scanner's own v1 resolution text for the same alias — when it carries a
    // real shape — re-anchors it as a literal at `anchor-backfill` origin.
    let backfill_texts = type_compat_v2::derive_backfill_texts(
        &type_resolution.explicit_manifest,
        &type_resolution.inferred_types,
    );
    match type_compat_v2::run_capture(
        sidecar,
        &capture_root,
        &service_id,
        &anchors,
        &backfill_texts,
        config.tsconfig.as_deref(),
    ) {
        Some((stub_dir, artifact)) => {
            cloud_data.capture_stub = Some(artifact);
            Some(stub_dir)
        }
        None => {
            // A capture degrades benignly when this service simply emitted no
            // usable stub. A capture that degraded because the sidecar is no
            // longer running is a different event: everything downstream of it
            // loses its types too. Probing separates the two — the capture call
            // itself only reports "no stub".
            match sidecar.health_check() {
                Ok(_) => {
                    if cloud_data.type_extraction_status.is_none() {
                        cloud_data.type_extraction_status = Some(
                            "v2 type capture degraded: no stub package was produced; cross-repo \
                             type compatibility for this service will report unverifiable"
                                .to_string(),
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Type capture failed and the sidecar is not responding: {}",
                        e
                    );
                    cloud_data.type_extraction_status = Some(format!(
                        "type capture failed: the type sidecar is no longer running ({})",
                        e
                    ));
                    cloud_data.types_degraded = Some(TypeDegradation {
                        stage: "capture".to_string(),
                        detail: e.to_string(),
                    });
                }
            }
            None
        }
    }
}

/// Discover files and extract symbols for MultiAgentOrchestrator
/// How many unresolved alias specifiers the report names before it truncates.
const MAX_NAMED_UNRESOLVED_SPECIFIERS: usize = 5;

/// State the imports the call graph could not follow (carrick#1104), so a
/// caller missing from `get_callers` has a line saying why. Deterministic and
/// repeatable, so it is a logged limit rather than a `scan_health` loss: a
/// repo that aliases through its bundler would otherwise be permanently red.
fn report_unresolved_imports(
    unresolved: &crate::call_graph::UnresolvedImports,
    unfollowed_extends: &[String],
) {
    if let Some(line) = undeclared_alias_line(unresolved) {
        info!("{line}");
    }
    for line in missing_mapping_lines(unresolved) {
        info!("{line}");
    }
    if unresolved.computed_requires > 0 {
        info!(
            "Call graph: {} require() call(s) take a computed specifier, so the module they load is unknown. Write the path as a string literal.",
            unresolved.computed_requires
        );
    }
    if unresolved.undeclared_packages > 0 {
        debug!(
            "Call graph: {} import(s) name a package no manifest declares (runtime builtins included), so those calls record no edge",
            unresolved.undeclared_packages
        );
    }
    if !unfollowed_extends.is_empty() {
        info!(
            "Call graph: tsconfig `extends` named no config on disk, so aliases it would inherit are not read: {}",
            unfollowed_extends.join(", ")
        );
    }
}

/// The specifiers no config declares, worst first, capped.
///
/// Same four things as [`missing_mapping_lines`] and for the same reason: how
/// many, which ones, what is missing, and what to do. What this costs the
/// scanner — that the calls record no caller edge — was half the old line and
/// is ours to know, not theirs to read (David's ruling, 2026-09-17).
///
/// The instruction carries the three config kinds that would fix it, which is
/// also the answer to the question the old line spent a clause on: an alias
/// set only in bundler or build code is not one of them, and declaring it in
/// one of these is what makes it readable.
fn undeclared_alias_line(unresolved: &crate::call_graph::UnresolvedImports) -> Option<String> {
    if unresolved.aliases.is_empty() {
        return None;
    }
    let imports: usize = unresolved.aliases.values().sum();
    let mut ranked: Vec<(&String, &usize)> = unresolved.aliases.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let named: Vec<&str> = ranked
        .iter()
        .take(MAX_NAMED_UNRESOLVED_SPECIFIERS)
        .map(|(specifier, _)| specifier.as_str())
        .collect();
    Some(format!(
        "Call graph: {imports} import(s) through {} undeclared alias(es) are unresolved: {}{}. Declare them in tsconfig, package.json or a Deno import map.",
        unresolved.aliases.len(),
        named.join(", "),
        if ranked.len() > named.len() {
            ", ..."
        } else {
            ""
        }
    ))
}

/// One line per config mapping whose target is not on disk, worst first
/// (carrick#1273).
///
/// Four things and nothing else: how many imports, which mapping, what is
/// missing, and what to do. A count with no referent is what the old `debug!`
/// line was and it told nobody anything — but explaining what an unresolved
/// import costs US is our internals, not their problem (David's ruling,
/// 2026-09-17).
///
/// The target named is the MAPPING's — `src/db/generated/client/`, not the
/// file one import happened to want under it. Whether it is there at all is
/// the difference between a build step nobody ran and one import spelled
/// wrong, and it is carried by the instruction rather than by a clause of its
/// own, which is where a reader needs it and costs no words.
///
/// Capped like its sibling above: a repo with dozens of broken mappings has
/// one cause, not dozens, and the ranked head names it.
fn missing_mapping_lines(unresolved: &crate::call_graph::UnresolvedImports) -> Vec<String> {
    let mut ranked: Vec<(&String, &crate::call_graph::MissingMapping)> =
        unresolved.missing_mappings.iter().collect();
    ranked.sort_by(|a, b| b.1.imports.cmp(&a.1.imports).then(a.0.cmp(b.0)));
    ranked
        .iter()
        .take(MAX_NAMED_UNRESOLVED_SPECIFIERS)
        .map(|(declared_by, missing)| {
            format!(
                "Call graph: {} import(s) through `{declared_by}` are unresolved: its target {} does not exist. {}",
                missing.imports,
                missing.target_root,
                if missing.directory_missing {
                    "Generate it, or fix the mapping."
                } else {
                    "Fix the mapping, or the import."
                },
            )
        })
        .collect()
}

fn discover_files_and_symbols(
    repo_path: &str,
    service: &Config,
    cm: Lrc<SourceMap>,
) -> FileDiscoveryResult {
    crate::scan_stage::enter(crate::scan_stage::Stage::Discovery);
    let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));
    let repo_name = get_repository_name(repo_path);

    // Find files scoped to this service's directory (+ include roots).
    let ignore_patterns = service_ignore_patterns(service);
    let walk_started = Instant::now();
    let (files, _) = find_service_files(repo_path, service, &ignore_patterns);
    // Timed separately from the parse that follows: the walk and the parse
    // scale with different things, and a scan that spends minutes in the walk
    // is a walk reaching somewhere it should not (carrick#751).
    info!(
        "Discovered {} file(s) under {} in {:.1}s",
        files.len(),
        service.directory.as_deref().unwrap_or("the repo root"),
        walk_started.elapsed().as_secs_f64()
    );

    // Zero files means the scan target is wrong (typo'd path, empty checkout):
    // proceeding would upload an empty service and silently erase its
    // coverage from the index.
    if files.is_empty() {
        let scope = match &service.directory {
            Some(dir) => format!("service directory '{}'", dir),
            None => "repository root".to_string(),
        };
        return Err(format!(
            "No JS/TS source files found under {} in '{}'. Check the scan path \
             and the directory/include entries in carrick.json.",
            scope, repo_path
        )
        .into());
    }

    debug!("Found {} files to analyze in {}", files.len(), repo_path);

    // Extract imported symbols and function definitions by parsing files.
    //
    // The service-wide sample is a set of import FACTS, not a local-name-keyed
    // map: two files importing different modules under the same local name are
    // both facts about this service, and collapsing them onto one key would
    // let walk order decide which module is in the framework-detect body at
    // all (carrick#954). The per-file maps below keep their local-name keying,
    // which is what call resolution needs.
    let mut all_import_facts = crate::framework_detector::ImportSample::default();
    // Definitions stay per file until every file has been parsed: the merge is
    // collision-aware (#582) and can only tell a colliding key from a unique
    // one once it can see them all.
    let mut per_file_definitions: Vec<(PathBuf, HashMap<String, FunctionDefinition>)> = Vec::new();
    // Per-file call-resolution inputs, kept beside the merged map because a
    // definition key means something only within its own file: resolving edges
    // against the merged map would make a correct edge depend on walk order.
    // Keyed by CANONICAL path, which is what import specifiers resolve to — a
    // repo reached through a symlink would otherwise miss every cross-file
    // lookup with no error.
    let mut per_file_calls: HashMap<PathBuf, crate::call_graph::FileCallIndex> = HashMap::new();
    // What each file's functions send, read while the module is in hand and
    // composed once call resolution has run (carrick#1555).
    let mut request_irs: HashMap<PathBuf, crate::request_summary::FileIr> = HashMap::new();
    // The imports those summaries read the value of, per file keyed like
    // `per_file_calls`: resolved by the call graph's own walk (carrick#1568).
    let mut wanted_from: Vec<(PathBuf, PathBuf)> = Vec::new();

    for file_path in &files {
        if let Some(module) = parse_file(file_path, &cm, &handler) {
            // Extract import symbols
            let mut import_extractor = ImportSymbolExtractor::new();
            module.visit_with(&mut import_extractor);
            let file_imports = import_extractor.imported_symbols;
            all_import_facts.add_file(&module, &file_imports);

            // Call resolution reads the `require` bindings too (carrick#1348).
            // Merged AFTER the sample above is taken: the analyzer's import
            // table is the ESM facts and nothing else.
            let (call_imports, computed_requires) =
                crate::call_graph::call_resolution_imports(&module, file_imports);

            // Extract function definitions with type annotations and source text
            let mut func_extractor =
                FunctionDefinitionExtractor::new(file_path.clone(), cm.clone());
            module.visit_with(&mut func_extractor);
            func_extractor.finalize_exports();

            let definition_keys: HashSet<String> = func_extractor
                .function_definitions
                .keys()
                .cloned()
                .collect();
            let request_ir =
                crate::request_summary::extract_file_ir(&module, &cm, &definition_keys);

            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.clone());
            wanted_from.push((canonical.clone(), file_path.clone()));
            request_irs.insert(file_path.clone(), request_ir);
            per_file_calls.insert(
                canonical,
                crate::call_graph::FileCallIndex {
                    path: file_path.clone(),
                    definitions: func_extractor
                        .function_definitions
                        .iter()
                        .map(|(key, def)| (key.clone(), def.line_number))
                        .collect(),
                    callees: func_extractor.callee_refs,
                    imports: call_imports,
                    computed_requires,
                    field_types: func_extractor.field_types,
                    instances: crate::receiver_type::module_scope_types(&module),
                },
            );
            per_file_definitions.push((file_path.clone(), func_extractor.function_definitions));
        }
    }

    // One row per definition, with same-named definitions in different files
    // re-keyed rather than collapsed onto each other (#582).
    let crate::call_graph::MergedDefinitions {
        definitions: mut all_function_definitions,
        keys,
    } = crate::call_graph::merge_definitions(per_file_definitions, repo_path);

    // Resolve the collected call sites into `FunctionDefinition::calls` while
    // the per-file scopes are still in hand. Deterministic and LLM-free, so it
    // runs on every path, `CARRICK_SKIP_INTENTS` included.
    //
    // The walk above is scoped to ONE service, so the manifest index is what
    // lets a call into a sibling workspace package resolve at all (carrick#776).
    // It also reads the aliases the repo's config declares, so a call imported
    // through `@/` resolves (carrick#1104). Call edges feed no analyzer input,
    // which is why only this index reads them; see
    // `docs/reference/module-resolution.md`.
    let repo_root = std::path::Path::new(repo_path);
    let service_tsconfig = service.alias_tsconfig();
    let workspace = crate::workspace_resolver::WorkspaceIndex::build_with_aliases(
        repo_root,
        service_tsconfig
            .as_ref()
            .map(|(directory, config)| (directory.as_path(), config.as_path())),
    );
    // Every specifier matters only where some module holds an instance
    // another may import: that is the only reading it can turn off.
    let every_specifier = request_irs
        .values()
        .any(crate::request_summary::FileIr::holds_instances);
    let bindings_wanted: HashMap<PathBuf, crate::call_graph::BindingsWanted> = wanted_from
        .into_iter()
        .map(|(canonical, walked)| {
            let wanted = request_irs[&walked].bindings_wanted(every_specifier);
            (canonical, wanted)
        })
        .collect();
    let resolution = crate::call_graph::resolve_call_edges(
        &mut all_function_definitions,
        &per_file_calls,
        &keys,
        &workspace,
        repo_root,
        &bindings_wanted,
    );
    report_unresolved_imports(&resolution.unresolved, &workspace.unfollowed_extends());

    // Composed later, once the library semantics this service can use are
    // verified: that needs detection and the service's sidecar, and both come
    // after discovery (carrick#1564).
    let request_inputs = crate::request_summary::RequestSummaryInputs {
        files: request_irs,
        sites: resolution.sites,
        bindings: resolution.bindings,
    };

    debug!(
        "Extracted {} import facts and {} function definitions from {} files",
        all_import_facts.fact_count(),
        all_function_definitions.len(),
        files.len()
    );

    Ok(FileDiscovery {
        files,
        import_facts: all_import_facts,
        function_definitions: all_function_definitions,
        repo_name,
        request_inputs,
    })
}

/// What discovery hands the request summaries and the library sites for the
/// service rooted at `root`, read with the default config: for tests of the
/// receiver core (carrick#1661).
#[cfg(test)]
pub(crate) fn discover_request_inputs(root: &Path) -> crate::request_summary::RequestSummaryInputs {
    let cm: Lrc<SourceMap> = Default::default();
    discover_files_and_symbols(&root.to_string_lossy(), &Config::default(), cm)
        .expect("discovery reads the service")
        .request_inputs
}

/// The service's request summaries (carrick#1555), composed with the library
/// semantics its installed packages verify (carrick#1564).
///
/// Verification runs on every scan and is never cached: it reads
/// `node_modules`, which the blob does not see. With no sidecar, no answered
/// semantics, or a sidecar that fails, the semantics are empty and the
/// summaries are exactly what they are without them.
fn summarize_requests(
    inputs: &crate::request_summary::RequestSummaryInputs,
    entries: Option<&[crate::client_semantics::ClientSemanticsEntry]>,
    sidecar: Option<&TypeSidecar>,
    service_root: &Path,
) -> crate::request_summary::RequestSummaryIndex {
    let semantics = match (entries, sidecar) {
        (Some(entries), Some(sidecar)) if !entries.is_empty() => {
            crate::client_semantics::verify(sidecar, service_root, entries)
        }
        _ => crate::client_semantics::LibrarySemantics::default(),
    };
    let summaries = crate::request_summary::summarize(inputs, &semantics);
    debug!(
        "request summaries: {} row(s) at call sites ({} through library semantics), {} site(s) whose callee sends nothing, {} request(s) with no statable URL",
        summaries.row_count(),
        summaries.library_row_count(),
        summaries.silent_count(),
        summaries.undetermined
    );
    summaries
}

/// A service's library semantics while the in-scan schedule settles them
/// (carrick#1564): known already, or being settled by a task that runs beside
/// the file analysis from the moment the model setup is known.
enum SettlingSemantics {
    Known(Option<Vec<crate::client_semantics::ClientSemanticsEntry>>),
    Settling {
        task: ScheduleTask,
        asked: Vec<crate::client_semantics::ClientSemanticsEntry>,
    },
}

/// The spawned schedule, aborted when dropped: a scan that stops before it
/// reads the summaries (interrupted, or failed) sends no further ask and
/// prints no further line.
struct ScheduleTask(tokio::task::JoinHandle<Vec<crate::client_semantics::ClientSemanticsEntry>>);

impl Drop for ScheduleTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl SettlingSemantics {
    /// The settled entries. A task that did not finish (it panicked, or the
    /// runtime is shutting down) leaves what the first ask gave.
    async fn settled(self) -> Option<Vec<crate::client_semantics::ClientSemanticsEntry>> {
        match self {
            Self::Known(entries) => entries,
            Self::Settling { mut task, asked } => match (&mut task.0).await {
                Ok(settled) => Some(settled),
                Err(error) => {
                    debug!(
                        "The in-scan library-semantics schedule did not finish ({error}); keeping what the first ask gave"
                    );
                    Some(asked)
                }
            },
        }
    }
}

/// Start the in-scan schedule for `detection` where `schedule` holds and an
/// installed package is still `pending`: every such package is asked about
/// again on [`crate::client_semantics::settle_pending`]'s schedule, one HTTP
/// attempt per ask, each bounded by
/// [`crate::client_semantics::PENDING_REASK_TIMEOUT`]. An answer gives its
/// semantics whatever lists it names, and nothing else ([`semantics_from_reask`]);
/// an ask that fails changes nothing, and the next scan asks again.
fn start_semantics_schedule(
    detection: &DetectionResult,
    schedule: bool,
    packages: &Packages,
    import_facts: &crate::framework_detector::ImportSample,
    service_root: &Path,
    repo_root: &Path,
) -> SettlingSemantics {
    let Some(asked) = detection.client_semantics.clone() else {
        return SettlingSemantics::Known(None);
    };
    if !schedule
        || !crate::client_semantics::wants_reask(
            Some(&asked),
            &detection.data_fetchers,
            service_root,
            repo_root,
        )
    {
        return SettlingSemantics::Known(Some(asked));
    }
    let packages = std::sync::Arc::new(packages.clone());
    let import_facts = std::sync::Arc::new(import_facts.clone());
    spawn_schedule(
        asked,
        crate::client_semantics::installed_for(service_root, repo_root),
        crate::client_semantics::pending_reask_waits(),
        move || {
            let (packages, import_facts) = (packages.clone(), import_facts.clone());
            async move {
                let detector = FrameworkDetector::new(reask_agent());
                semantics_from_reask(
                    detector.detect_frameworks_and_libraries(&packages, &import_facts),
                )
                .await
            }
        },
        say_schedule,
    )
}

/// Spawn [`crate::client_semantics::settle_pending`] over `asked`, so it runs
/// beside the analysis whatever the analysis blocks on. Aborted when the
/// returned value is dropped ([`ScheduleTask`]), and quiet about its own
/// panics ([`crate::panic_report::quiet`]): the schedule is best effort, so
/// a failure inside it keeps what the first ask gave and never reports the
/// scan as failed.
fn spawn_schedule<Ask, AskFut, Shown>(
    asked: Vec<crate::client_semantics::ClientSemanticsEntry>,
    installed: impl Fn(&str) -> bool + Send + Sync + 'static,
    waits: Vec<std::time::Duration>,
    ask: Ask,
    notice: impl FnMut(crate::client_semantics::ScheduleNotice) -> Shown + Send + 'static,
) -> SettlingSemantics
where
    Ask: FnMut() -> AskFut + Send + 'static,
    AskFut: std::future::Future<Output = Option<Vec<crate::client_semantics::ClientSemanticsEntry>>>
        + Send,
    Shown: Send + 'static,
{
    let entries = asked.clone();
    let task = tokio::spawn(crate::panic_report::quiet(async move {
        crate::client_semantics::settle_pending(
            entries,
            installed,
            &waits,
            ask,
            tokio::time::sleep,
            notice,
        )
        .await
    }));
    SettlingSemantics::Settling {
        task: ScheduleTask(task),
        asked,
    }
}

/// Once the library semantics have settled, send the request summaries
/// composed with them, and return the settled entries for the blob to keep.
/// Runs alongside the file analysis, which reads the summaries only once the
/// model has been asked. Always sends.
async fn compose_summaries(
    inputs: &crate::request_summary::RequestSummaryInputs,
    settling: SettlingSemantics,
    sidecar: Option<&TypeSidecar>,
    service_root: &Path,
    summaries: tokio::sync::oneshot::Sender<crate::request_summary::RequestSummaryIndex>,
) -> Option<Vec<crate::client_semantics::ClientSemanticsEntry>> {
    let settled = settling.settled().await;
    let index = summarize_requests(inputs, settled.as_deref(), sidecar, service_root);
    if summaries.send(index).is_err() {
        debug!("The analysis ended before its request summaries were composed");
    }
    settled
}

/// The library semantics one in-scan re-ask gives: what `ask` answered, when
/// it answered within [`crate::client_semantics::PENDING_REASK_TIMEOUT`],
/// whatever lists it names (carrick#1606): the schedule fills only entries
/// still `pending`, by package name. A failure or no answer in time gives
/// nothing, and the next scan asks again.
pub(crate) async fn semantics_from_reask<E: std::fmt::Display>(
    ask: impl std::future::Future<Output = Result<DetectionResult, E>>,
) -> Option<Vec<crate::client_semantics::ClientSemanticsEntry>> {
    match tokio::time::timeout(crate::client_semantics::PENDING_REASK_TIMEOUT, ask).await {
        Ok(Ok(fresh)) => fresh.client_semantics,
        Ok(Err(error)) => {
            debug!("Asking again for library semantics failed ({error})");
            None
        }
        Err(_) => {
            debug!("Asking again for library semantics ran out of time");
            None
        }
    }
}

/// Whether the in-scan schedule may ask about library semantics: a model is
/// in use, the run is not collecting prompts, and the service's previous
/// generation is not this run's own (a retry of owed work asked minutes ago).
///
/// A service whose model stages were deferred needs no rule here: its setup
/// carries the empty detection ([`ModelSetup::deferred`]), so there is
/// nothing pending to settle (the `a_deferred_service_runs_no_schedule` test).
fn semantics_schedule_applies(generation: PreviousGeneration) -> bool {
    !crate::local_mode::no_model()
        && generation == PreviousGeneration::Stored
        && !crate::analysis_channel::dispatching()
}

/// Resolve a repo's `carrick.json` into one service config per service.
///
/// An explicit config wins. Without one, the shared resolver derives services
/// from workspace manifests, or returns one service for a plain repository.
///
/// A config that exists but cannot be parsed, or that declares paths that
/// don't exist, is a hard error: silently falling back to defaults would
/// ignore the user's declared service layout and upload a wrong index.
fn resolve_services(repo_path: &str) -> Result<Vec<Config>, Box<dyn std::error::Error>> {
    crate::service_derivation::resolve(std::path::Path::new(repo_path))
        .map(|derived| derived.services)
        .map_err(Into::into)
}

/// Build-artifact directories to skip everywhere.
fn service_ignore_patterns(_service: &Config) -> Vec<&'static str> {
    crate::packages::MANIFEST_SKIP_DIRS.to_vec()
}

/// Load the package data for a single service, scoped to its own
/// `package.json` (within its directory), not an arbitrary one from the repo.
///
/// A missing `package.json` is fine (empty dependency set); one that exists
/// but cannot be parsed is a hard error — defaulting to zero dependencies
/// would silently gut framework detection and endpoint extraction.
fn load_packages_for_service(
    repo_path: &str,
    service: &Config,
) -> Result<Packages, Box<dyn std::error::Error>> {
    let package_json_path =
        crate::file_finder::find_service_manifest(std::path::Path::new(repo_path), service);
    let mut manifests: Vec<PathBuf> = package_json_path.into_iter().collect();
    if let Some(config) = crate::deno_support::service_manifest(Path::new(repo_path), service) {
        // Root imports apply to members. Preserve the existing Packages
        // dependency-version selection policy across inherited manifests.
        let mut inherited: Vec<_> = config
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .take_while(|p| p.starts_with(repo_path))
            .filter_map(crate::deno_support::manifest_at)
            .collect();
        inherited.reverse();
        inherited.extend(manifests);
        manifests = inherited;
        let mut seen = std::collections::HashSet::new();
        manifests.retain(|p| seen.insert(p.clone()));
    }
    let mut packages = Packages::new(manifests)?;
    // Names of every package.json in the WHOLE repo tree (not just this
    // service's): a workspace member like `packages/contracts` is not a
    // service, but a dependency on it is internal, not a registry package.
    packages.internal_names =
        crate::packages::collect_internal_package_names(std::path::Path::new(repo_path));
    Ok(packages)
}

/// Merge provenance entries into an existing list, deduped on
/// `(path, reason)` and sorted by path.
///
/// Two layers report here and they see different things: the inferrer knows
/// why it declined to read a payload, the capture self-check knows which
/// members of the emitted declaration are `any`. Neither subsumes the other,
/// so both are kept — but the same finding must not appear twice, and the
/// order must not depend on which layer ran first, or `scan-twice.sh`
/// byte-identity fails.
fn merge_any_provenance(
    into: &mut Vec<crate::services::type_sidecar::TypeProvenance>,
    incoming: impl IntoIterator<Item = crate::services::type_sidecar::TypeProvenance>,
) {
    for item in incoming {
        if into
            .iter()
            .any(|existing| existing.path == item.path && existing.reason == item.reason)
        {
            continue;
        }
        into.push(item);
    }
    into.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));
}

/// Resolve per-endpoint type definitions from the v2 capture stub tree.
/// Populates `resolved_definition` and `expanded_definition` on each manifest entry.
/// Non-fatal: if resolution fails, entries keep their None values and the MCP falls back to regex.
fn resolve_per_endpoint_definitions(
    sidecar: &TypeSidecar,
    cloud_data: &mut CloudRepoData,
    stub_dir: &Path,
) {
    crate::scan_stage::enter(crate::scan_stage::Stage::Definitions);
    let Some(ref mut manifest) = cloud_data.type_manifest else {
        return;
    };

    let records = read_capture_records(stub_dir);
    let aliases = aliases_to_resolve(manifest, &records);
    let resolved = if aliases.is_empty() {
        Vec::new()
    } else {
        debug!(
            "Resolving {} type definition(s) via compiler",
            aliases.len()
        );
        match sidecar.resolve_definitions(&stub_dir.to_string_lossy(), &aliases) {
            Ok(resolved) => resolved,
            Err(e) => {
                warn!("Per-endpoint definition resolution failed: {}", e);
                debug!("Continuing without resolved definitions (MCP will use regex fallback)");
                Vec::new()
            }
        }
    };
    let count = join_capture_answers(manifest, resolved, &records);
    debug!("Resolved {} type definition(s)", count);
}

/// Write the capture's answers onto the manifest, then its provenance.
///
/// Provenance is stamped for EVERY entry, including the ones whose
/// `type_state` is Unknown and therefore print no definition at all: "no type
/// here, and here is why" is the answer a reader needs (carrick#376).
///
/// It is stamped after the answers settle the state, not before. The
/// declared-open-member rule (carrick#1752) settles an answer whose every
/// top type it can explain, and a position the emitted tree cannot resolve
/// (carrick#1446) prints as a name, not as a top type: there is nothing in the
/// text for it to explain, and the same answer without a declared member
/// settles on its text alone. The record's own `any_provenance` reaches that
/// rule directly, so the order moves nothing else.
///
/// Returns how many aliases the capture answered for.
fn join_capture_answers(
    manifest: &mut [TypeManifestEntry],
    resolved: Vec<crate::services::type_sidecar::ResolvedDefinitionResult>,
    records: &HashMap<String, crate::services::type_sidecar::CaptureAliasRecord>,
) -> usize {
    let count = apply_resolved_definitions(manifest, resolved, records);
    stamp_capture_provenance(manifest, records);
    count
}

/// The aliases to ask the capture stub about: every entry in the manifest.
///
/// The filter this used to carry asked only about entries v1 carried a type
/// for, plus the ones v1 abstained on with a BARE top type. An entry whose v1
/// answer was a real shape with a top type somewhere INSIDE it fell between the
/// two — `contains_disqualifying_top_type` demoted it to `Unknown`, nothing
/// marked it an abstention, and the capture was never asked (carrick#1441). On
/// one real monorepo that silently discarded a clean capture answer for 48
/// entries. There is nothing to gain by guessing which aliases are worth
/// asking about: the answers are filtered on their own merits below, the call
/// is one round trip, and the capture exists precisely to resolve what v1
/// could not.
///
/// A `BTreeSet`, not a `HashSet`: the sidecar resolves the aliases in the order
/// they arrive, and the compiler hands out type ids in the order it creates
/// types, so the request order used to reach the printed form of a union
/// (carrick#735). A `HashSet` iterates under a per-process random seed, which
/// made that order — and the bytes of the request — differ between two runs of
/// the same binary over an unchanged tree.
///
/// Beside each entry, the capture alias that carries its unwidened reading
/// (carrick#1516) when the capture recorded one.
fn aliases_to_resolve(
    manifest: &[TypeManifestEntry],
    records: &HashMap<String, crate::services::type_sidecar::CaptureAliasRecord>,
) -> Vec<String> {
    manifest
        .iter()
        .flat_map(|e| {
            let unwidened = type_compat_v2::unwidened_alias(&e.type_alias);
            let captured = records.contains_key(&unwidened);
            std::iter::once(e.type_alias.clone()).chain(captured.then_some(unwidened))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Write the capture's answers onto the manifest, and let a real shape settle
/// the state of an entry the v1 side did not answer.
///
/// Two different questions are asked of the capture's answer for such an entry,
/// and they have different thresholds:
///
///  - **is there anything to publish?** An answer that is itself a bare `any`
///    or `unknown` describes nothing, so it is not written: the entry keeps no
///    definition, and its `any_provenance` says why. Writing it would publish
///    `any` — "this endpoint accepts anything" — where the truth is "no layer
///    could see this type". A shape with a top type somewhere INSIDE it still
///    describes a payload and is published, with its provenance (carrick#376).
///    This question is asked of EVERY entry, not only the abstained ones: a
///    non-answer is not worth publishing whoever asked for it, and the readers
///    that count typed operations count `resolved_definition.is_some()`
///    (carrick#852).
///
///    Nor is an answer whose capture record says it names something that does
///    not resolve (carrick#1165): an identifier nothing declares, a declaration
///    whose own import is missing, or a top type with no pinned external to
///    heal it. Those print as a confident name (`Row`,
///    `Shape<Options>`) where the truth is that the stub cannot say what the
///    name is, and publishing them counts the operation typed.
///  - **does it settle the state?** A shape with no disqualifying top type
///    anywhere in it, the same notion the check phase uses, or one whose every
///    top type the capture's record shows is a member the source declares
///    `unknown` (carrick#1752): a typed contract with an open field is still a
///    typed contract. That promotion is scoped to entries whose state is
///    `Unknown`: the ones v1 never answered, and the ones v1 answered with a
///    shape whose text carried a top type it could not attribute
///    (carrick#449, carrick#1441). The second kind returns to the state v1
///    gave it, because whether the source annotated the type is a fact about
///    the source, not about which layer printed the shape; the first kind
///    reads `Implicit`. What the SOURCE states about an entry v1 answered
///    cleanly is never overwritten from here.
///
/// Returns how many aliases the capture answered for.
fn apply_resolved_definitions(
    manifest: &mut [TypeManifestEntry],
    resolved: Vec<crate::services::type_sidecar::ResolvedDefinitionResult>,
    records: &HashMap<String, crate::services::type_sidecar::CaptureAliasRecord>,
) -> usize {
    let lookup: HashMap<String, _> = resolved
        .into_iter()
        .map(|r| (r.type_alias.clone(), r))
        .collect();
    for entry in manifest.iter_mut() {
        let Some(r) = lookup.get(&entry.type_alias) else {
            continue;
        };
        // "No layer has stated this type yet" — the v1 side either abstained,
        // answered with something `contains_disqualifying_top_type` refused, or
        // was never asked. All three leave the entry `Unknown`, and the capture
        // is the layer that answers for all three (carrick#1441).
        let v1_unanswered = entry.type_state == ManifestTypeState::Unknown;

        // An answer that IS a bare top type describes nothing, and describes
        // nothing whoever asked for it. The rule used to be scoped to entries
        // v1 abstained on, so an entry v1 DID answer for published
        // `export type … = unknown;` as its resolved definition — and every
        // reader that asks "does this operation have a type" asks
        // `resolved_definition.is_some()`, so a non-answer was counted as an
        // answer (carrick#852). Publishing it also offers a producer side to a
        // compatibility check with nothing in it.
        //
        // The empty answer stays scoped to the unanswered entry: an entry v1
        // answered for has a shape behind it, and an empty expansion there is
        // the capture failing to print one rather than the capture saying there
        // is none.
        if type_compat_v2::text_is_bare_top_type(&r.expanded)
            || (v1_unanswered && r.expanded.trim().is_empty())
        {
            continue;
        }
        if let Some(reason) = records
            .get(&entry.type_alias)
            .and_then(|record| record.unpublishable_reason())
        {
            debug!(
                "Not publishing the capture's answer for {}: {reason}",
                entry.type_alias
            );
            continue;
        }

        entry.resolved_definition = Some(r.definition.clone());
        entry.expanded_definition = Some(r.expanded.clone());
        entry.unwidened_definition = lookup
            .get(&type_compat_v2::unwidened_alias(&entry.type_alias))
            .filter(|u| {
                !type_compat_v2::contains_disqualifying_top_type(&u.expanded)
                    && u.expanded.trim() != r.expanded.trim()
                    && !u.expanded.trim().is_empty()
                    && records
                        .get(&u.type_alias)
                        .is_none_or(|record| record.unpublishable_reason().is_none())
            })
            .map(|u| u.expanded.clone());

        let settles = !type_compat_v2::contains_disqualifying_top_type(&r.expanded)
            || records.get(&entry.type_alias).is_some_and(|record| {
                type_compat_v2::top_types_are_declared_open_members(
                    &r.expanded,
                    record,
                    &entry.any_provenance,
                )
            });
        if v1_unanswered && settles {
            let state = entry
                .v1_state_before_demotion
                .unwrap_or(ManifestTypeState::Implicit);
            let is_explicit = state == ManifestTypeState::Explicit;
            entry.is_explicit = is_explicit;
            entry.type_state = state;
            entry.evidence.is_explicit = is_explicit;
            entry.evidence.type_state = state;
        }
    }
    lookup.len()
}

/// The capture's per-alias records, keyed by alias.
///
/// The stub's own `carrick-manifest.json` is the record of what the emitted
/// declaration tree actually says, which is what the index publishes. Non-fatal
/// throughout: no manifest or an unreadable one reads as no records, and every
/// reader treats an alias with no record as "the capture said nothing", never
/// as a claim of cleanliness.
fn read_capture_records(
    stub_dir: &Path,
) -> HashMap<String, crate::services::type_sidecar::CaptureAliasRecord> {
    let path = stub_dir.join(CAPTURE_MANIFEST_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    #[derive(serde::Deserialize)]
    struct StubManifest {
        #[serde(default)]
        aliases: Vec<crate::services::type_sidecar::CaptureAliasRecord>,
    }
    let Ok(parsed) = serde_json::from_str::<StubManifest>(&text) else {
        warn!(
            "capture manifest at {} could not be parsed for provenance",
            path.display()
        );
        return HashMap::new();
    };
    parsed
        .aliases
        .into_iter()
        .map(|record| (record.alias.clone(), record))
        .collect()
}

/// Join the capture self-check's findings onto the manifest: the capture record
/// is the right source for "which fields of this endpoint's type are `any`".
/// Both of its lists join: the `any`/`unknown` the emitted declarations state,
/// and the positions the emitted tree cannot resolve, which print as a name
/// and read `any` (carrick#1446). An alias with no record leaves its entry as
/// it was.
fn stamp_capture_provenance(
    manifest: &mut [TypeManifestEntry],
    records: &HashMap<String, crate::services::type_sidecar::CaptureAliasRecord>,
) {
    for entry in manifest.iter_mut() {
        let Some(record) = records.get(&entry.type_alias) else {
            continue;
        };
        merge_any_provenance(
            &mut entry.any_provenance,
            record
                .any_provenance
                .iter()
                .chain(&record.unresolved_in_tree)
                .cloned(),
        );
    }
}

fn build_type_manifest_entries(
    mount_graph: &MountGraph,
    config: &Config,
    repo_root: &str,
) -> Vec<TypeManifestEntry> {
    let normalizer = UrlNormalizer::new(config);
    let mut entries = Vec::new();

    // A producer's alias carries its DECLARATION SITE, the way a consumer's
    // carries its call site (carrick#718). Two endpoints on one (method, path)
    // — a pathless layout and the page beneath it, an `index` module and its
    // sibling, or a mis-extracted duplicate route (#332) — therefore get an
    // entry each and resolve their own type. Until they did, they shared one
    // alias, one of them was dropped here to stop the other's definition being
    // clobbered in the bundle (#334), and which one survived depended on the
    // order this loop saw them in.
    for endpoint in mount_graph.get_resolved_endpoints() {
        let method = normalize_manifest_method(&endpoint.method);
        if !is_producer_method(&method) {
            continue;
        }
        // Call-site-evidence entries (#379) never anchor Producer types: they
        // are client encodings of an external contract, and a producer
        // manifest entry would make the type check run a request-vs-request
        // comparison mislabelled as a producer-contract verdict. Their pairs
        // are verdict-exempt at the source; the site's Consumer entry is
        // still emitted from the twin data call below.
        if endpoint.evidence == carrick_match::MatchEvidence::CallSite {
            continue;
        }
        let path = endpoint.full_path.clone();
        if !path.starts_with('/') {
            continue;
        }
        let (file_path, line_number) = parse_file_location(&endpoint.file_location);

        let key = OperationKey::http(&method, path);
        let site_id = build_site_id(&file_path, line_number, &key, repo_root);

        add_manifest_pair(
            &mut entries,
            key,
            ManifestRole::Producer,
            &file_path,
            line_number,
            Some(&site_id),
        );
    }

    for call in mount_graph.get_data_calls() {
        if !normalizer.is_probable_url(&call.target_url) {
            continue;
        }
        let (file_path, line_number) = parse_file_location(&call.file_location);
        let method = normalize_manifest_method(&call.method);
        if !is_http_method(&method) {
            continue;
        }
        // Key on the canonical path computed once at mount-graph build time, so
        // the manifest join key is byte-identical to the projection key.
        let path = call.canonical_path.clone();
        // Only anchor types for a BARE route (internal/declared or relative
        // target). An external or unclassified call keeps its raw `${HOST}/path`
        // / full-URL canonical form: it has no internal producer to match, so a
        // type anchor would be unused. Mirrors main, where the raw projection
        // key never joined an external call's `extract_path`-keyed manifest
        // entry.
        if !path.starts_with('/') {
            continue;
        }
        let key = OperationKey::http(&method, path);
        let call_id = build_site_id(&file_path, line_number, &key, repo_root);

        add_manifest_pair(
            &mut entries,
            key,
            ManifestRole::Consumer,
            &file_path,
            line_number,
            Some(&call_id),
        );
    }

    entries
}

/// Drop the consumer type entries a row stated at a call to a function the
/// service declares does not state.
///
/// - **Response**, for a row the request summaries restate at a caller
///   (carrick#1601): the call's value is what the function returns, not the
///   response body.
/// - **Request**, for a row whose function builds the body itself or sends
///   none ([`crate::forwarded_body::CallBody::Built`], carrick#1782): the
///   call hands the function its parameters, and the function's own request
///   line states the body. What the caller passes is an argument, not the
///   request body. A row whose function sends a declared parameter unchanged
///   keeps its entry, typed from that declaration.
///
/// An entry left at `unknown` would still make the site a party to the type
/// check, read `unverifiable` there and carry that up to its pair, so the
/// entry goes, as it does for a call with no internal producer.
///
/// Joined to the call rows the way [`stamp_manifest_anchor_symbols`] joins
/// them, by `(file_path, line)`, and by the verb, so another row on the same
/// line keeps its entries.
fn drop_call_through_entries(
    manifest: &mut Vec<TypeManifestEntry>,
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
) {
    let normalize_line = |line: i32| -> u32 { if line <= 0 { 1 } else { line as u32 } };
    let unstated: HashSet<(&str, u32, String, ManifestTypeKind)> = file_results
        .iter()
        .flat_map(|(file_path, result)| {
            result.data_calls.iter().flat_map(move |call| {
                let response = call.at_caller.then_some(ManifestTypeKind::Response);
                let request = (call.call_body == Some(crate::forwarded_body::CallBody::Built))
                    .then_some(ManifestTypeKind::Request);
                response.into_iter().chain(request).map(move |kind| {
                    (
                        file_path.as_str(),
                        normalize_line(call.line_number),
                        normalize_manifest_method(call.method.as_deref().unwrap_or_default()),
                        kind,
                    )
                })
            })
        })
        .collect();
    if unstated.is_empty() {
        return;
    }
    manifest.retain(|entry| {
        let dropped = entry.role == ManifestRole::Consumer
            && entry.key.as_http().is_some_and(|(method, _)| {
                unstated.contains(&(
                    entry.file_path.as_str(),
                    entry.line_number,
                    method.to_string(),
                    entry.type_kind,
                ))
            });
        !dropped
    });
}

/// Thread the LLM's real type-anchor symbol onto the manifest entries (#233).
///
/// The manifest's `type_alias` is a synthetic hashed name (`Endpoint_<hash>_…`);
/// the real source symbol (`StatusResponse`) lives on the file-analyzer result.
/// Join by `(file_path, line_number)`: the mount-graph `file_location` the
/// manifest is built from is `"{file_path}:{line}"`, where `file_path` is the
/// same key `file_results` is keyed by and `line` is the same LLM-emitted
/// `line_number`. Stamp the symbol onto every manifest entry for that op
/// (request + response) so the eval projection surfaces the real anchor instead
/// of the hash. The first non-None symbol per `(file, line)` wins.
///
/// The symbol-side line is normalized exactly as the manifest side
/// (`parse_file_location`): a non-positive/missing line collapses to `1`. Keying
/// a line-0 anchor at `0` would never join a manifest entry (always `>= 1`), so
/// the anchor would silently fall back to the hashed `type_alias`.
/// The anchor a file-analyzer result stated for one source site: the symbol,
/// and the import specifier the file brought it in through.
struct AnchorAtSite {
    symbol: String,
    import_source: Option<String>,
}

fn stamp_manifest_anchor_symbols(
    manifest: &mut [TypeManifestEntry],
    file_results: &HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult>,
    repo_path: &str,
    modules: &crate::workspace_resolver::WorkspaceIndex,
) {
    // Mirror `parse_file_location`'s `Some(0) | None => 1` normalization so the
    // join keys line up on both sides.
    let normalize_line = |line: i32| -> u32 { if line <= 0 { 1 } else { line as u32 } };
    // (file_path, line_number) -> the anchor, from endpoints and calls.
    let mut symbols: HashMap<(String, u32), AnchorAtSite> = HashMap::new();
    for (file_path, result) in file_results {
        for endpoint in &result.endpoints {
            if let Some(symbol) = endpoint.primary_type_symbol.as_ref() {
                symbols
                    .entry((file_path.clone(), normalize_line(endpoint.line_number)))
                    .or_insert_with(|| AnchorAtSite {
                        symbol: symbol.clone(),
                        import_source: endpoint.type_import_source.clone(),
                    });
            }
        }
        for call in &result.data_calls {
            if let Some(symbol) = call.primary_type_symbol.as_ref() {
                symbols
                    .entry((file_path.clone(), normalize_line(call.line_number)))
                    .or_insert_with(|| AnchorAtSite {
                        symbol: symbol.clone(),
                        import_source: call.type_import_source.clone(),
                    });
            }
        }
    }
    if symbols.is_empty() {
        return;
    }
    // One parse of a declaring file serves every entry anchored in it, and one
    // failed resolution is remembered as a failure rather than retried.
    let mut homes: HashMap<(String, String), Option<crate::cloud_storage::TypeHome>> =
        HashMap::new();
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    for entry in manifest.iter_mut() {
        let Some(anchor) = symbols.get(&(entry.file_path.clone(), entry.line_number)) else {
            continue;
        };
        entry.primary_type_symbol = Some(anchor.symbol.clone());
        entry.defined_in = homes
            .entry((entry.file_path.clone(), anchor.symbol.clone()))
            .or_insert_with(|| {
                anchor_declaration_site(&entry.file_path, anchor, repo_path, modules, &cm, &handler)
            })
            .clone();
    }
}

/// Where the file that USES an anchor says the anchor is declared, confirmed
/// against the declaring file's own AST (carrick#649).
///
/// The import specifier the analyzer recorded is resolved exactly as the type
/// sidecar's own `SymbolRequest` resolves it (`resolve_import_path`), so the
/// two agree on which file the symbol was expected in. A specifier that names
/// no file on disk resolves to a path that does not exist and is dropped here
/// rather than reported; so is a file that turns out not to declare the symbol,
/// which is what a barrel re-export looks like from the outside.
///
/// With no import specifier the symbol is declared in the using file itself, if
/// anywhere — and the declaration line is still worth stating, because the
/// entry's own `line_number` is the operation's line, not the type's.
fn anchor_declaration_site(
    using_file: &str,
    anchor: &AnchorAtSite,
    repo_path: &str,
    modules: &crate::workspace_resolver::WorkspaceIndex,
    cm: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<crate::cloud_storage::TypeHome> {
    let using_absolute = absolute_source_path(using_file, repo_path);
    let declaring = match anchor.import_source.as_deref() {
        Some(source) => PathBuf::from(
            crate::agents::file_orchestrator::FileOrchestrator::resolve_import_path(
                &using_absolute.to_string_lossy(),
                source,
                modules,
            ),
        ),
        None => using_absolute,
    };
    if !declaring.is_file() {
        return None;
    }
    let line =
        crate::type_manifest::type_declaration_line(&declaring, &anchor.symbol, cm, handler)?;
    Some(crate::cloud_storage::TypeHome {
        file_path: repo_relative(&declaring.to_string_lossy(), repo_path),
        line_number: line,
        symbol: anchor.symbol.clone(),
    })
}

/// A manifest `file_path` as an absolute path on disk. The manifest carries
/// whatever form the mount graph's `file_location` had, which is repo-relative
/// for an uploaded blob and absolute for a live pass; joining an already
/// absolute path onto the root is a no-op, so one branch covers both.
fn absolute_source_path(file_path: &str, repo_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(repo_path).join(path)
    }
}

/// Take back a model anchor the type arbitration rejected (carrick#1779).
///
/// `stamp_manifest_anchor_symbols` writes the model's symbol onto every entry
/// of an op's site before any type resolves. When
/// `demote_witnessed_borrowed_anchors` then drops or re-aims the explicit
/// request that symbol made, the row serves a type the symbol does not name:
/// the wrapper the source casts its body read to, say, with the anchor still
/// naming the element inside it. Each change reaches every entry of the same
/// op at the same site that still carries the rejected symbol, the request
/// entry included, since the stamp wrote it there too. A re-aimed root
/// becomes the anchor, with the declaration the sidecar found for it. A
/// dropped one leaves no anchor and no home, so enrichment fills the anchor
/// from each entry's own inference.
fn restamp_arbitrated_anchors(
    manifest: &mut [TypeManifestEntry],
    changes: &[crate::services::type_sidecar::AnchorChange],
    repo_path: &str,
) {
    if changes.is_empty() {
        return;
    }
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    for change in changes {
        let Some((key, role, file_path, line_number)) = manifest
            .iter()
            .find(|entry| entry.type_alias == change.alias)
            .map(|entry| {
                (
                    entry.key.clone(),
                    entry.role,
                    entry.file_path.clone(),
                    entry.line_number,
                )
            })
        else {
            continue;
        };
        let home = change
            .reaimed
            .as_ref()
            .and_then(|root| reported_declaration_home(root, repo_path, &cm, &handler));
        for entry in manifest.iter_mut().filter(|entry| {
            entry.key == key
                && entry.role == role
                && entry.file_path == file_path
                && entry.line_number == line_number
                && entry.primary_type_symbol.as_deref() == Some(change.rejected.as_str())
        }) {
            entry.primary_type_symbol = change.reaimed.as_ref().map(|root| root.symbol.clone());
            entry.defined_in = home.clone();
        }
    }
}

/// Where the sidecar says a re-aimed root is declared, as the manifest states
/// a home: repo-relative, at the declaration's line. The sidecar's path is
/// absolute and may spell the repo root in its resolved form, so both forms
/// are tried; a file outside the repo, or one that does not declare the
/// symbol under that name, has no home to state.
fn reported_declaration_home(
    root: &crate::services::type_sidecar::AnchorRoot,
    repo_path: &str,
    cm: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<crate::cloud_storage::TypeHome> {
    let declaring = Path::new(&root.source_file);
    if !declaring.is_file() {
        return None;
    }
    let line = crate::type_manifest::type_declaration_line(declaring, &root.symbol, cm, handler)?;
    let inside = |base: &Path| {
        declaring
            .strip_prefix(base)
            .ok()
            .filter(|rest| rest.is_relative())
            .map(Path::to_path_buf)
    };
    let repo = Path::new(repo_path);
    let relative = inside(repo).or_else(|| inside(&repo.canonicalize().ok()?))?;
    Some(crate::cloud_storage::TypeHome {
        file_path: relative.to_string_lossy().replace('\\', "/"),
        line_number: line,
        symbol: root.symbol.clone(),
    })
}

/// Anchor a consumer response at the type its source states the body to be
/// (carrick#1817).
///
/// A call's inference reports `stated_body` when the source casts or
/// annotates the body it reads (`(await res.json()) as MemberPage`), and the
/// row serves that statement. Its root is the named type the data-call schema
/// asks the model for. The inference's own anchor cannot stand in for it: it
/// is the call's type, `Response` for `fetch`, which enrichment's fill never
/// writes. So a response entry left without an anchor, because the model
/// named none or because `restamp_arbitrated_anchors` took back one the
/// arbitration dropped, is anchored at the stated root, with its declaration.
/// It runs after the restamp and before enrichment, whose fill then reaches
/// only the entries this leaves without an anchor.
///
/// The entry keeps no anchor when the root is:
/// - written with type arguments (`Page<Order>`): what such a row should
///   name is not decided;
/// - not declared in the repo under that name, so no home can be stated
///   (the compiler's own globals, such as `Blob`, fall here);
/// - transport machinery, which never anchors a row.
fn anchor_stated_body_roots(
    manifest: &mut [TypeManifestEntry],
    inferred: &[crate::services::type_sidecar::InferredType],
    repo_path: &str,
) {
    // The first inference per alias is the one whose text the entry serves,
    // as in enrichment.
    let mut first_by_alias: HashMap<&str, &crate::services::type_sidecar::InferredType> =
        HashMap::new();
    for inf in inferred {
        first_by_alias.entry(inf.alias.as_str()).or_insert(inf);
    }
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    for entry in manifest
        .iter_mut()
        .filter(|entry| entry.primary_type_symbol.is_none())
    {
        let Some(inf) = first_by_alias.get(entry.type_alias.as_str()) else {
            continue;
        };
        let Some(stated) = inf
            .stated_body
            .as_ref()
            .filter(|stated| stated.root_type_arguments.is_none())
        else {
            continue;
        };
        let (Some(root), Some(source_file)) = (stated.root.as_ref(), stated.root_source.as_ref())
        else {
            continue;
        };
        if TypeSidecar::is_untyped_response_type(root) {
            continue;
        }
        let root = crate::services::type_sidecar::AnchorRoot {
            symbol: root.clone(),
            source_file: source_file.clone(),
        };
        let Some(home) = reported_declaration_home(&root, repo_path, &cm, &handler) else {
            continue;
        };
        entry.primary_type_symbol = Some(root.symbol);
        entry.defined_in = Some(home);
    }
}

fn add_manifest_pair(
    entries: &mut Vec<TypeManifestEntry>,
    key: OperationKey,
    role: ManifestRole,
    file_path: &str,
    line_number: u32,
    site_id: Option<&str>,
) {
    // Producers for GET/HEAD/OPTIONS never have request bodies
    let skip_request = role == ManifestRole::Producer
        && matches!(
            key.as_http().map(|(method, _)| method),
            Some("GET" | "HEAD" | "OPTIONS")
        );

    for type_kind in [ManifestTypeKind::Request, ManifestTypeKind::Response] {
        if skip_request && type_kind == ManifestTypeKind::Request {
            continue;
        }
        let type_alias = build_manifest_type_alias_with_site_id(&key, role, type_kind, site_id);
        let infer_kind = infer_kind_for_manifest(role, type_kind);
        let evidence = crate::cloud_storage::TypeEvidence {
            file_path: file_path.to_string(),
            span_start: None,
            span_end: None,
            line_number,
            infer_kind,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
        };
        entries.push(TypeManifestEntry {
            key: key.clone(),
            role,
            type_kind,
            type_alias,
            file_path: file_path.to_string(),
            line_number,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
            evidence,
            resolved_definition: None,
            expanded_definition: None,
            // Threaded on after the fact by `stamp_manifest_anchor_symbols`,
            // which joins the LLM's real anchor symbol by `(file_path, line)`.
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            unwidened_definition: None,
            v1_state_before_demotion: None,
        });
    }
}

fn infer_kind_for_manifest(role: ManifestRole, type_kind: ManifestTypeKind) -> InferKind {
    match (role, type_kind) {
        (ManifestRole::Consumer, ManifestTypeKind::Response) => InferKind::CallResult,
        (_, ManifestTypeKind::Response) => InferKind::ResponseBody,
        (_, ManifestTypeKind::Request) => InferKind::RequestBody,
    }
}

/// Enrich manifest entries with type resolution results.
///
/// This function updates the `type_state` and `is_explicit` fields of manifest entries
/// based on the results from the TypeSidecar. Types that were successfully resolved
/// (either explicitly or through inference) will have their state updated from `Unknown`
/// to `Explicit` or `Implicit`.
fn enrich_manifest_with_type_resolution(
    manifest: &mut [TypeManifestEntry],
    type_resolution: &TypeResolutionResult,
    bundled_dts: Option<&str>,
) {
    // Build a lookup of resolved type aliases
    // Key: type_alias, Value: (type_string, is_explicit)
    let mut resolved_types: HashMap<String, (String, bool)> = HashMap::new();

    // Add explicit types from the manifest
    for entry in &type_resolution.explicit_manifest {
        resolved_types.insert(entry.alias.clone(), (entry.type_string.clone(), true));
    }

    // Add inferred types
    for inferred in &type_resolution.inferred_types {
        // Don't overwrite explicit types with inferred ones
        if !resolved_types.contains_key(&inferred.alias) {
            resolved_types.insert(
                inferred.alias.clone(),
                (inferred.type_string.clone(), inferred.is_explicit),
            );
        }
    }

    // Deterministic anchor source (#240): the sidecar resolves each inferred
    // type's real source symbol (`Payment`) off the ts-morph `Type`, so a
    // manifest entry whose anchor the LLM left unset can be filled from it.
    // Join by `alias` — the same key the resolved-type lookup below uses to
    // marry an `InferredType` to its manifest entry. A `(file_path, line)` join
    // would be fragile: the sidecar's `source_location` is an absolute ts-morph
    // path while `entry.file_path` is repo-relative, so the coordinates need not
    // line up. First non-None wins per alias, so a later inferred entry can't
    // clobber an earlier real symbol.
    //
    // Transport machinery never anchors a row (carrick#1779). A consumer
    // call's inference anchors on the call's own type, which for `fetch` is
    // `Response`: the thing the body is read out of, not the body.
    let mut inferred_symbols: HashMap<String, String> = HashMap::new();
    for inferred in &type_resolution.inferred_types {
        if let Some(symbol) = inferred
            .primary_type_symbol
            .as_ref()
            .filter(|symbol| !TypeSidecar::is_untyped_response_type(symbol))
        {
            inferred_symbols
                .entry(inferred.alias.clone())
                .or_insert_with(|| symbol.clone());
        }
    }

    // Why an inference came back `any` (carrick#376). Joined on the same
    // `alias` key as everything else here. The inferrer is the only layer that
    // knows the difference between a type that IS `any` and a recovery that
    // declined to guess, so its reason travels rather than being reconstructed
    // from the text downstream.
    let mut inferred_provenance: HashMap<
        String,
        Vec<crate::services::type_sidecar::TypeProvenance>,
    > = HashMap::new();
    for inferred in &type_resolution.inferred_types {
        if inferred.any_provenance.is_empty() {
            continue;
        }
        inferred_provenance
            .entry(inferred.alias.clone())
            .or_insert_with(|| inferred.any_provenance.clone());
    }

    // Also check the bundled .d.ts content for defined types
    // This catches types that were successfully bundled but not in the manifest.
    // Exclude aliases defined as `= unknown` — those are placeholders for failed
    // inferences and should not be promoted to Implicit.
    let dts_defined_aliases: HashSet<String> = if let Some(dts) = bundled_dts {
        manifest
            .iter()
            .filter(|e| {
                dts_defines_alias(dts, &e.type_alias)
                    && !dts_alias_is_trivially_unknown(dts, &e.type_alias)
            })
            .map(|e| e.type_alias.clone())
            .collect()
    } else {
        HashSet::new()
    };

    // A type is genuinely unresolved when it resolves to `unknown`/`any`/empty,
    // or when the bundled .d.ts only carries the marked `= unknown` placeholder
    // Carrick writes for an alias no shape reached. Either way the shape never
    // reached the bundle, so the entry must read `Unknown` — never a promoted
    // state that asserts a shape we don't actually have.
    let dts_trivially_unknown = |alias: &str| {
        bundled_dts
            .map(|dts| dts_alias_is_trivially_unknown(dts, alias))
            .unwrap_or(false)
    };

    // Update manifest entries
    for entry in manifest.iter_mut() {
        // Fill the deterministic anchor ONLY when the LLM left it unset, so the
        // ops where the model already emitted a correct symbol (POST /payments,
        // socket) are never regressed. Stamping runs before enrichment, so any
        // entry still `None` here had no LLM anchor.
        if entry.primary_type_symbol.is_none()
            && let Some(symbol) = inferred_symbols.get(&entry.type_alias)
        {
            entry.primary_type_symbol = Some(symbol.clone());
        }

        if let Some(provenance) = inferred_provenance.get(&entry.type_alias) {
            merge_any_provenance(&mut entry.any_provenance, provenance.iter().cloned());
        }

        if let Some((type_string, is_explicit)) = resolved_types.get(&entry.type_alias) {
            // Check if the type is actually resolved. The shared
            // disqualifying-top-type notion (adversarial-review finding 2)
            // rejects not just whole-string "any"/"unknown" but any/unknown
            // at ANY position — `any[]`, `Promise<any>`, `Record<string,
            // any>`, `{ metadata: any }`, `{ getData: () => any }`. Since
            // carrick #448 removed the `type_state == Unknown` pre-verdict,
            // `check_v2` is the sole authority on resolved-ness, so its capture
            // self-check deep walk (`findDisqualifyingTopType`) is a genuine
            // SUPERSET of this text scan — with two deliberate, safe
            // exceptions the walk does NOT flag: a callable PARAMETER `any`
            // (contravariant, genuinely permissive — this text scan may flag
            // it, but abstaining there would only over-demote, never
            // false-compatible), and TypeScript's unresolved-external `error`
            // placeholder (heals when the check installs the pin). So the two
            // layers agree on every author-baked disqualifier; they diverge
            // only in the fail-closed direction.
            let placeholder = type_string.trim().is_empty()
                || type_compat_v2::text_is_bare_top_type(type_string)
                || dts_trivially_unknown(&entry.type_alias);
            let top_type_inside = type_compat_v2::contains_disqualifying_top_type(type_string);

            if placeholder || top_type_inside {
                // Downgrade to Unknown so the `= unknown` placeholder gate
                // (resolve_per_endpoint_definitions, check_v2 pre-gates) stays shut and the
                // edge is reported unverifiable rather than falsely compatible.
                //
                // A real shape with a top type inside it keeps what v1 said
                // about it beside the demotion: whether the source wrote that
                // top type is the capture record's to say, and when it did,
                // the entry returns to this state (carrick#1752).
                entry.v1_state_before_demotion = (!placeholder).then_some(if *is_explicit {
                    ManifestTypeState::Explicit
                } else {
                    ManifestTypeState::Implicit
                });
                entry.is_explicit = false;
                entry.type_state = ManifestTypeState::Unknown;
                entry.evidence.is_explicit = false;
                entry.evidence.type_state = ManifestTypeState::Unknown;
            } else {
                entry.is_explicit = *is_explicit;
                entry.type_state = if *is_explicit {
                    ManifestTypeState::Explicit
                } else {
                    ManifestTypeState::Implicit
                };
                entry.evidence.is_explicit = *is_explicit;
                entry.evidence.type_state = entry.type_state;
            }
        } else if dts_defined_aliases.contains(&entry.type_alias) {
            // Type is defined in the .d.ts but wasn't in our resolution results
            // This can happen for inline aliases or other edge cases
            entry.type_state = ManifestTypeState::Implicit;
            entry.evidence.type_state = ManifestTypeState::Implicit;
        } else if dts_trivially_unknown(&entry.type_alias) {
            // Only a `= unknown` placeholder reached the bundle — keep Unknown.
            entry.is_explicit = false;
            entry.type_state = ManifestTypeState::Unknown;
            entry.evidence.is_explicit = false;
            entry.evidence.type_state = ManifestTypeState::Unknown;
        }
    }

    // Log enrichment stats
    let explicit_count = manifest
        .iter()
        .filter(|e| e.type_state == ManifestTypeState::Explicit)
        .count();
    let implicit_count = manifest
        .iter()
        .filter(|e| e.type_state == ManifestTypeState::Implicit)
        .count();
    let unknown_count = manifest
        .iter()
        .filter(|e| e.type_state == ManifestTypeState::Unknown)
        .count();
    debug!(
        "Manifest enrichment: {} explicit, {} implicit, {} unknown",
        explicit_count, implicit_count, unknown_count
    );
}

/// What one service's discovery produced: the files, the import facts and the
/// function definitions, with the source map their spans belong to.
///
/// Discovery is the whole-service SWC parse, the call-graph pass and the
/// manifest walk. The incremental path runs it before it knows whether it can
/// reuse anything, so the full path it falls back to takes this rather than
/// discovering again (carrick#1108).
struct Discovered {
    cm: Lrc<SourceMap>,
    files: Vec<PathBuf>,
    import_facts: crate::framework_detector::ImportSample,
    function_definitions: HashMap<String, FunctionDefinition>,
    repo_name: String,
    request_inputs: crate::request_summary::RequestSummaryInputs,
}

/// The full analysis of one service over its discovery.
///
/// `repo_path` is canonical: the caller canonicalised it before discovering,
/// so the paths in `discovered` and the paths normalised here agree.
#[allow(clippy::too_many_arguments)]
async fn analyze_current_repo(
    repo_path: &str,
    service: &Config,
    packages: &Packages,
    sidecar: Option<&TypeSidecar>,
    discovered: Discovered,
    previous_intents: PreviousIntents,
    settled: Option<SettledDetection>,
    workspace: &mut crate::external_call_candidates::WorkspaceScan,
    run_intents: &RunIntentMemo,
    graphql_schemas: &crate::graphql::SchemaCatalogue,
) -> Result<ServiceAnalysis, Box<dyn std::error::Error>> {
    debug!("Running multi-agent analysis on: {}", repo_path);

    let config = service;

    let Discovered {
        cm,
        files,
        import_facts: all_import_facts,
        function_definitions,
        repo_name,
        request_inputs,
    } = discovered;
    debug!(
        "Repository '{}': {} files, {} function definitions",
        repo_name,
        files.len(),
        function_definitions.len()
    );

    // Function intents need only discovery's definitions and the previous
    // scan's hashes, so they run beside the multi-agent analysis rather than
    // after it (carrick#1065). Even on a full scan, intents whose content hash
    // matches the previous scan are reused: the intent cache is
    // content-addressed and independent of the analysis cache's validity (see
    // the caller).
    let intents = IntentsInFlight::start(
        AgentService::new(),
        function_definitions,
        previous_intents,
        run_intents.clone(),
    );

    // 3. Create MultiAgentOrchestrator (auth is via GitHub Actions OIDC)
    let orchestrator = MultiAgentOrchestrator::new(cm.clone());

    // 3b. Settle the model stages for this service: detection and guidance
    // (or a deferral), plus the extraction config. Local mode asks nothing.
    let mut setup = if crate::local_mode::no_model() {
        debug!("Local mode: skipping framework detection and guidance (no model)");
        ModelSetup::ready(
            DetectionResult::default(),
            crate::local_mode::offline_guidance(),
            None,
        )
    } else {
        model_setup(packages, &all_import_facts, settled).await
    };
    // The in-scan schedule starts now, before the GraphQL hints below, so it
    // runs beside everything up to the point the analysis reads the
    // summaries (carrick#1564).
    let settling = start_semantics_schedule(
        &setup.detection,
        semantics_schedule_applies(PreviousGeneration::Stored),
        packages,
        &all_import_facts,
        &service_scan_root(repo_path, config),
        Path::new(repo_path),
    );

    // Stage B2: derive the GraphQL producer field-list from the service's SDL
    // (deterministic, cheap) so the file-analyzer can link resolver functions to
    // schema fields and emit `graphql_operations`. Empty (a no-op) for non-GraphQL
    // services. `append_deterministic_protocol_operations` scans the SDL again
    // later for the operation index; the duplicate parse is acceptable.
    let graphql_producer_hints = crate::graphql::GraphqlProducerHints::collect(
        service_graphql_roots(repo_path, service),
        &crate::graphql::resolve_declared_schemas(Path::new(repo_path), &service.graphql_schemas)
            .files,
        &files,
    );
    // #268: the consumer mirror — document consumers with no deterministic
    // call-site anchor, so the file-analyzer can locate their co-located
    // result type. Same duplicate-parse tradeoff as the producer hints above.
    let graphql_consumer_hints = crate::graphql::GraphqlConsumerHints::collect(
        service_graphql_roots(repo_path, service),
        &files,
        repo_path,
    );

    // 4. Run the complete multi-agent analysis
    let normalizer = UrlNormalizer::new(config);
    let service_root = service_scan_root(repo_path, config);
    // Composed here rather than in discovery: the library semantics they read
    // through exist only once detection has answered (carrick#1564).
    let (summaries_sender, summaries) = tokio::sync::oneshot::channel();
    // One index for this service's join passes and, below, its type requests
    // and anchor stamps (carrick#1416).
    let service_modules = service_module_index(repo_path, config);
    enter_stage(crate::scan_stage::Stage::FileAnalysis)?;
    // The summaries are composed once the schedule started above has
    // settled; the analysis reads them only once the model has been asked
    // (carrick#1564).
    let service_root_text = service_root.to_string_lossy().into_owned();
    let library_ask = LibraryAsk::start(
        &request_inputs,
        sidecar,
        &service_root,
        Path::new(repo_path),
    );
    let (analysis_result, settled, library_answer) = tokio::join!(
        orchestrator.run_complete_analysis(
            files.clone(),
            packages,
            &setup,
            &service_root_text,
            Path::new(repo_path),
            &graphql_producer_hints,
            &graphql_consumer_hints,
            &normalizer,
            &service_modules,
            sidecar,
            crate::agents::file_orchestrator::SummarySource::later(summaries),
        ),
        compose_summaries(
            &request_inputs,
            settling,
            sidecar,
            &service_root,
            summaries_sender,
        ),
        first_library_answer(library_ask.as_ref()),
    );
    let analysis_result = analysis_result?;
    setup.detection.client_semantics = settled;
    crate::phase_timing::mark(crate::phase_timing::Phase::Model);

    // 4b. Collect the function intents started after discovery. `Intents`
    // times only the wait left at this point; the rest overlapped the model
    // stage above.
    enter_stage(crate::scan_stage::Stage::Intents)?;
    let mut function_definitions = intents.finish().await;
    crate::phase_timing::mark(crate::phase_timing::Phase::Intents);

    enter_stage(crate::scan_stage::Stage::Signatures)?;
    // 4c. Compose function signatures, inferring unannotated slots via sidecar.
    populate_function_signatures(
        signature_sidecar(sidecar),
        &mut function_definitions,
        repo_path,
    );
    crate::phase_timing::mark(crate::phase_timing::Phase::Signatures);
    enter_stage(crate::scan_stage::Stage::BlobBuild)?;

    // 4d. Deterministic protocol scans run BEFORE the graph is projected: the
    // GraphQL consumer file set folds transport data calls out of the mount
    // graph (#307) so every downstream surface (cloud projection, type
    // manifest, type requests) sees the same call set.
    let (mut protocol_extractions, document_sites) = scan_protocol_extractions(
        repo_path,
        service,
        &files,
        &analysis_result.file_results,
        &setup.detection.socket_clients,
    );
    let mut analysis_result = analysis_result;
    settle_graphql_documents(
        &mut protocol_extractions.graphql,
        document_sites,
        &analysis_result.file_results,
        repo_path,
        &service_modules,
        &mut analysis_result.mount_graph,
        service,
        graphql_schemas,
    );
    protocol_extractions.library =
        library_rows(library_ask.as_ref(), library_answer, sidecar, &service_root).await;
    let withdrawn = withdraw_model_routes_at_definitions(
        &mut analysis_result.mount_graph,
        &analysis_result.file_results,
        &protocol_extractions.library,
        repo_path,
    );
    if withdrawn > 0 {
        debug!("Model routes withdrawn at library definitions: {withdrawn}");
    }
    // The repo's own statement of what a body-dispatching handler serves
    // (carrick#831), applied after every pass that reads the source: a
    // declaration replaces what inference produced for the same handler, so
    // it has to run once inference is finished with the graph.
    let declared =
        crate::dispatch::apply_declared_operations(&mut analysis_result.mount_graph, service);
    if declared > 0 {
        debug!("Declared operations materialised from carrick.json: {declared}");
    }
    let analysis_result = analysis_result;
    crate::phase_timing::mark(crate::phase_timing::Phase::Protocols);

    // Cloud-bound paths must be repo-relative. The incremental path gets
    // this from build_cloud_data_from_mount_graph; this full path constructs
    // CloudRepoData directly, so relativize here (after signatures are
    // composed — they embed the same absolute prefix).
    relativize_function_definition_paths(
        &mut function_definitions,
        repo_path,
        &served_paths::PathScrub::for_scan(repo_path),
    );

    // 5. Build CloudRepoData directly from multi-agent results (bypassing Analyzer adapter layer)
    let mut cloud_data = CloudRepoData::from_multi_agent_results(
        repo_name.clone(),
        repo_path,
        &analysis_result,
        serde_json::to_string(config).ok(),
        serde_json::to_string(packages).ok(),
        Some(packages.clone()),
        function_definitions,
    );
    let in_process_pubsub = classify_in_process_pubsub(
        repo_path,
        &analysis_result.file_results,
        &cloud_data,
        &service_modules,
        &setup.detection,
        &protocol_extractions.event_bus,
    );
    append_deterministic_protocol_operations(
        &mut cloud_data,
        &protocol_extractions,
        &analysis_result.file_results,
        &in_process_pubsub,
        repo_path,
        service,
    );
    attach_external_call_candidates(&mut cloud_data, repo_path, &files, config, workspace);
    attach_sdk_surface(&mut cloud_data, repo_path, config);
    crate::phase_timing::mark(crate::phase_timing::Phase::Surface);

    // The type requests and anchor stamps below read the same index the
    // analysis above was given: the specifier a type was imported by is the
    // repo's config to resolve (carrick#1416).
    let mut manifest_entries =
        build_type_manifest_entries(&analysis_result.mount_graph, config, repo_path);
    drop_call_through_entries(&mut manifest_entries, &analysis_result.file_results);
    stamp_manifest_anchor_symbols(
        &mut manifest_entries,
        &analysis_result.file_results,
        repo_path,
        &service_modules,
    );
    let library_sites = LibrarySiteIndex::of(
        &stated_library_rows(&protocol_extractions.library, repo_path, service),
        repo_path,
    );
    append_protocol_manifest_entries(&mut manifest_entries, &protocol_extractions, &library_sites);
    append_pubsub_manifest_entries(
        &mut manifest_entries,
        &analysis_result.file_results,
        &protocol_extractions.sockets,
        &in_process_pubsub,
        &library_sites,
        repo_path,
    );
    if !manifest_entries.is_empty() {
        cloud_data.type_manifest = Some(manifest_entries);
    }

    // 6. Resolve types using sidecar if available
    let agent_service = AgentService::new();
    let file_orchestrator = FileOrchestrator::new(agent_service.clone());

    let extraction_config = setup.extraction_config.clone();

    // Socket payload anchors and GraphQL consumer result-type anchors both
    // resolve through the same sidecar bundle path as HTTP explicit symbols
    // (#245/#248). Concatenate both into the extra-explicit slice.
    let mut protocol_requests = file_orchestrator.collect_socket_type_requests(
        &library_sites.typed_sockets(&protocol_extractions.sockets),
        repo_path,
        &service_modules,
    );
    protocol_requests.extend(file_orchestrator.collect_graphql_type_requests(
        &protocol_extractions.graphql,
        repo_path,
        &service_modules,
    ));
    // Pub/sub ops are LLM-sourced in `analysis_result.file_results`, not in the
    // deterministic `protocol_extractions`, so their payload anchors bundle
    // through the same path (#corpus-2 resolution dim). A row withdrawn as
    // in-process has no operation to type (carrick#1513).
    let pubsub_results = in_process_pubsub.pubsub_rows_kept(&analysis_result.file_results);
    protocol_requests.extend(file_orchestrator.collect_pubsub_type_requests(
        &pubsub_results,
        repo_path,
        &service_modules,
    ));

    // GraphQL producers take the infer path, not the bundle path: their response
    // contract is the resolver's expanded RETURN type, so they become
    // `FunctionReturn` infer requests (Stage B1).
    let mut protocol_infer = file_orchestrator.collect_graphql_producer_infer_requests(
        &protocol_extractions.graphql,
        repo_path,
        &service_modules,
    );
    // Pub/sub payloads with no named symbol (wrapper patterns: topic-map
    // emitters, schema-catalog workers, generic channel handles) resolve via
    // the LLM-located payload expression through the same infer path.
    protocol_infer
        .extend(file_orchestrator.collect_pubsub_infer_requests(&pubsub_results, repo_path));
    // A GraphQL consumer row whose executed document declares the field's
    // result type reads that type where it is declared (carrick#1761).
    protocol_infer.extend(
        file_orchestrator.collect_graphql_consumer_infer_requests(&protocol_extractions.graphql),
    );

    crate::phase_timing::mark(crate::phase_timing::Phase::Manifest);

    let stub_dir = resolve_types_if_available(
        sidecar,
        &file_orchestrator,
        &analysis_result.file_results,
        repo_path,
        extraction_config.as_ref(),
        &analysis_result.mount_graph,
        config,
        &service_modules,
        &protocol_requests,
        &protocol_infer,
        &mut cloud_data,
    );

    crate::phase_timing::mark(crate::phase_timing::Phase::Types);

    if let Some(bundled_types) = cloud_data.bundled_types.take() {
        let updated = append_missing_aliases(bundled_types, cloud_data.type_manifest.as_ref());
        cloud_data.bundled_types = Some(updated);
    }

    // 6b. Resolve per-endpoint definitions from the capture stub tree
    if let (Some(sidecar), Some(stub_dir)) = (sidecar, stub_dir.as_deref()) {
        resolve_per_endpoint_definitions(sidecar, &mut cloud_data, stub_dir);
    }
    if let Some(stub_dir) = &stub_dir {
        let _ = std::fs::remove_dir_all(stub_dir);
    }
    crate::phase_timing::mark(crate::phase_timing::Phase::Definitions);

    // 7. Populate cache fields for future incremental runs. The MODEL's raw
    // answers, verbatim — the same rule the incremental branch caches under
    // (see CACHE_VERSION).
    cloud_data.file_results = Some(normalize_file_results_keys(
        &analysis_result.raw_model_results,
        repo_path,
    ));
    // Handlers that switch on a request field (carrick#831). Read from the
    // NORMALIZED copy so the file path on a table is repo-relative like every
    // other path in the payload.
    cloud_data.dispatch_tables = cloud_data
        .file_results
        .as_ref()
        .and_then(crate::dispatch::collect_dispatch_tables);
    if let Some(tables) = cloud_data.dispatch_tables.clone() {
        let stamped = crate::dispatch::stamp_dispatch_tables_on_functions(
            &mut cloud_data.function_definitions,
            &tables,
        );
        debug!("Dispatch tables stamped onto function rows: {stamped}");
    }
    // Detection, guidance and extraction config, or none of them for a
    // deferred service (see the incremental branch).
    setup.stamp_cache(&mut cloud_data);
    cloud_data.cache_version = Some(CACHE_VERSION);
    // Same workspace-wide hash the incremental gate compares against.
    cloud_data.package_json_hash = Some(hash_workspace_package_jsons(packages, repo_path)?);

    // 8. Last step: make every path in the payload repo-relative. This branch
    // builds its mount graph from `file_results` keyed by as-scanned ABSOLUTE
    // paths (the incremental branch normalizes those keys before rebuilding the
    // graph; step 7 above normalizes only the cached copy, too late for the
    // projections), so without this the same repo uploads absolute locations on
    // a full scan and relative ones on an incremental scan.
    relativize_cloud_paths(
        &mut cloud_data,
        repo_path,
        &served_paths::PathScrub::for_scan(repo_path),
    );
    // Each route's handler function, placed once paths are relative so a row's
    // file and a definition's file compare (cloud#948).
    let placed = crate::handler_span::attach_handler_spans(&mut cloud_data);
    debug!("Handler spans placed on endpoint rows: {placed}");

    // 9. What this scan could not classify, stated beside what it did
    // (carrick#705). Collected last, off the finished payload and the stats of
    // the scan that filled it, so the reason lists quote the repo-relative
    // paths the index carries. The SDK half is folded in after the cross-repo
    // join, which is the only place a peer's surface is known.
    cloud_data.boundary = Some(crate::boundary::ServiceBoundary::collect(
        &cloud_data,
        &analysis_result.stats,
        &analysis_result.file_results,
        repo_path,
    ));
    crate::phase_timing::mark(crate::phase_timing::Phase::Other);

    Ok(ServiceAnalysis {
        data: cloud_data,
        deferred: setup.deferred,
    })
}

/// Main's copy of this repo and the peers it is matched against, kept aside
/// on a PR run with a prior index (carrick-cloud#1369).
struct MainBaselineInput {
    peers: Vec<CloudRepoData>,
    main_self: Vec<CloudRepoData>,
    /// This run scanned exactly what main's copy describes.
    surface_unchanged: bool,
    /// Whether main's copy is main as this PR's base has it
    /// (carrick-cloud#1408).
    copy: crate::pr_baseline::MainCopy,
}

/// How long main's side of the comparison may take before the run gives up on
/// it and posts every finding without the field. Overridable for tests.
const MAIN_SIDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAIN_SIDE_TIMEOUT_ENV: &str = "CARRICK_MAIN_SIDE_TIMEOUT_SECS";

fn main_side_timeout() -> std::time::Duration {
    std::env::var(MAIN_SIDE_TIMEOUT_ENV)
        .ok()
        .and_then(|secs| secs.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(MAIN_SIDE_TIMEOUT)
}

/// Mark each of the PR run's findings with whether main's copy of this repo,
/// matched against the same peers, yields it too (carrick-cloud#1369).
///
/// Skipped when no finding carries a pairing to mark. When this run scanned
/// exactly what main's copy describes, every finding is main's and is marked
/// so without running anything. Otherwise main's side runs under a timeout
/// and a panic guard: whatever goes wrong there is stated in one line and
/// every finding says the run could not tell (carrick-cloud#1408). It never
/// costs the run its PR result.
async fn mark_findings_on_main(
    findings: &mut [crate::findings::Finding],
    input: MainBaselineInput,
    sidecar: Option<&TypeSidecar>,
) {
    if !crate::pr_baseline::has_anything_to_mark(findings) {
        debug!("No mismatch to compare with main");
        return;
    }
    if input.surface_unchanged {
        info!("This PR leaves every scanned service as main has it: its findings are main's");
        crate::pr_baseline::mark_all_on_main(findings);
        return;
    }

    let copy = input.copy;
    let sp = logging::spinner("Comparing findings with main...");
    let timeout = main_side_timeout();
    let outcome = {
        let _caught = crate::panic_report::CaughtPanics::begin();
        tokio::time::timeout(
            timeout,
            futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(main_side_findings(
                input, sidecar,
            ))),
        )
        .await
    };
    let failure = match outcome {
        Ok(Ok(Ok(main))) => {
            crate::pr_baseline::mark_on_main(findings, &main, copy);
            logging::finish_spinner(&sp, "Compared findings with main");
            return;
        }
        Ok(Ok(Err(e))) => format!("main's analysis failed: {e}"),
        Ok(Err(panic)) => format!(
            "main's analysis panicked: {}",
            crate::pr_baseline::panic_message(panic.as_ref())
        ),
        Err(_) => format!("main's analysis took longer than {}s", timeout.as_secs()),
    };
    logging::finish_spinner_warn(&sp, "Could not compare findings with main");
    warn!("Findings are posted as not compared with main: {failure}");
    crate::pr_baseline::mark_all_unknown(findings, crate::findings::OnMainUnknown::MainSideFailed);
}

/// Main's side of the comparison: the same analysis this run's findings came
/// from, over main's stored copy and the same peers. Returns main's findings
/// and whether its type check ran.
async fn main_side_findings(
    input: MainBaselineInput,
    sidecar: Option<&TypeSidecar>,
) -> Result<crate::pr_baseline::MainSide, Box<dyn std::error::Error>> {
    crate::pr_baseline::record_main_side_run();
    match crate::pr_baseline::take_mock_main_side_fault() {
        Some(crate::pr_baseline::MainSideFault::Error) => {
            return Err("injected main-side failure".into());
        }
        Some(crate::pr_baseline::MainSideFault::Panic) => panic!("injected main-side panic"),
        Some(crate::pr_baseline::MainSideFault::Hang) => {
            std::future::pending::<()>().await;
        }
        None => {}
    }
    let MainBaselineInput {
        peers, main_self, ..
    } = input;
    // Main's own answers, from the scan that had main's sources on disk. The
    // recomputation below has none, so a response main's scan judged by
    // retyping the consumer's call (carrick#1491) is reached only here.
    let stored = crate::pr_baseline::stored_type_sites(&main_self);
    let sdk_input = crate::sdk_edges::SdkJoinInput::collect(
        peers.iter().chain(main_self.iter()),
        main_self.iter(),
    );
    let (analyzer, type_check) = build_cross_repo_analyzer(peers, main_self, sidecar).await?;
    let main_results = analyzer.get_results();
    let sdk_join = crate::sdk_edges::join(
        &sdk_input,
        &main_results.cross_repo_matches,
        &analyzer.pair_directions(),
    );
    let mut findings = main_results.findings;
    findings.extend(crate::sdk_edges::type_mismatch_findings(sdk_join.edges()));
    Ok(crate::pr_baseline::MainSide {
        findings,
        recomputed: analyzer.type_sites(),
        stored,
        types_judged: type_check == crate::local_mode::JoinTypeCheck::Ran,
    })
}

async fn build_cross_repo_analyzer(
    mut all_repo_data: Vec<CloudRepoData>,
    current_repos: Vec<CloudRepoData>,
    sidecar: Option<&TypeSidecar>,
) -> Result<(Analyzer, crate::local_mode::JoinTypeCheck), Box<dyn std::error::Error>> {
    // Add the freshly-analyzed local services (one per service) to the mix
    all_repo_data.extend(current_repos);
    // 1. Merge configs using generic function. (The v1 merged-packages value
    //    fed only the deleted ts_check install; the v2 check workspace pins
    //    each stub's own deps instead.)
    let combined_config = merge_serialized_data(&all_repo_data, |data| data.config_json.as_ref())?;

    // 2. Build analyzer using shared logic (skip type resolution for cross-repo)
    let cm: Lrc<SourceMap> = Default::default();
    let builder = AnalyzerBuilder::new_for_cross_repo(combined_config, cm);
    let mut analyzer = builder.build_from_repo_data(all_repo_data.clone()).await?;

    // 3. Merge mount graphs from all repos for framework-agnostic analysis
    let merged_mount_graph = MountGraph::merge_from_repos(&all_repo_data);
    analyzer.set_mount_graph(merged_mount_graph);

    // 4. Add packages data from all repos for dependency analysis. Key by
    //    service identity (service_name, falling back to repo_name) so two
    //    services in the same monorepo don't overwrite each other — matching
    //    the cloud's service_name ?? repo_name attribution convention.
    for repo_data in &all_repo_data {
        if let Some(packages) = &repo_data.packages {
            let key = repo_data
                .service_name
                .clone()
                .unwrap_or_else(|| repo_data.repo_name.clone());
            analyzer.add_repo_packages(key, packages.clone());
        }
    }

    // 5. Merged manifests power the alias -> display-name map for findings.
    let merged_manifests: Vec<TypeManifestEntry> = all_repo_data
        .iter()
        .filter_map(|repo| repo.type_manifest.as_ref())
        .flat_map(|entries| entries.iter().cloned())
        .collect();
    analyzer.set_type_manifests(merged_manifests);

    // 6. Run the v2 type check (capture stubs -> synthetic workspace -> tsc
    //    probes) and store the structured pair outcomes for the verdict
    //    overlay. Without a sidecar, compat is NOT evaluated: outcomes stay
    //    unset and every edge keeps `type_compatible: None` — the harness
    //    greps for this exact "Skipping type checking" trap (§7).
    let type_check = if let Some(sidecar) = sidecar {
        let local_consumers = type_compat_v2::take_local_consumers();
        let (outcomes, state) = finish_type_check(|| {
            type_compat_v2::run_check(sidecar, &all_repo_data, &local_consumers)
        });
        if let Some(outcomes) = outcomes {
            analyzer.set_pair_outcomes(outcomes);
        }
        state
    } else {
        warn!(
            "Skipping type checking: the type sidecar is unavailable, so v2 \
             capture/check cannot run. Compat verdicts will be absent, not 'compatible'."
        );
        crate::local_mode::JoinTypeCheck::Skipped {
            reason: crate::scan_health::types_unavailable_reason(crate::scan_health::WHOLE_SCAN)
                .unwrap_or_else(|| "the type sidecar is unavailable".to_string()),
        }
    };

    Ok((analyzer, type_check))
}

/// Run the type check and say whether it finished (carrick#1490).
///
/// "Ran" is what the check did, not whether a sidecar existed: a check that
/// panics leaves no outcomes and is reported as not run, with the panic as the
/// reason, instead of taking the whole cross-repo analysis down with it.
fn finish_type_check(
    run: impl FnOnce() -> Vec<crate::analyzer::PairCheckOutcome>,
) -> (
    Option<Vec<crate::analyzer::PairCheckOutcome>>,
    crate::local_mode::JoinTypeCheck,
) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(outcomes) => (Some(outcomes), crate::local_mode::JoinTypeCheck::Ran),
        Err(payload) => {
            let cause = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("no message");
            let reason = format!("the type check failed ({cause})");
            warn!("Skipping type checking: {reason}");
            (None, crate::local_mode::JoinTypeCheck::Skipped { reason })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::git_state::tests::committed_repo;

    /// A module resolver over a tree with no config: these fixtures name
    /// every file by path, so no alias mapping is in play (carrick#1416).
    fn modules_without_config() -> crate::workspace_resolver::WorkspaceIndex {
        let empty = tempfile::tempdir().expect("tempdir");
        crate::workspace_resolver::WorkspaceIndex::build_with_aliases(empty.path(), None)
    }

    /// carrick#1490: "ran" is what the check did. A check that panics is
    /// reported as not run, with its reason, and leaves no outcomes behind;
    /// one that returns is "ran" whatever it judged.
    #[test]
    fn a_type_check_that_fails_is_reported_as_not_run_with_its_reason() {
        let (outcomes, state) = super::finish_type_check(|| panic!("tsc exited 137"));
        assert!(outcomes.is_none());
        assert_eq!(
            state,
            crate::local_mode::JoinTypeCheck::Skipped {
                reason: "the type check failed (tsc exited 137)".to_string()
            }
        );

        let (outcomes, state) = super::finish_type_check(Vec::new);
        assert_eq!(outcomes.map(|o| o.len()), Some(0));
        assert_eq!(state, crate::local_mode::JoinTypeCheck::Ran);
    }

    /// The aliased-specifier line, pinned for the same reason as its sibling
    /// below: it carried the same internals clause and the same stray ticket
    /// ref, and was the last verbose line in this reporter.
    #[test]
    fn undeclared_aliases_are_named_with_their_count_and_what_to_do() {
        let mut unresolved = crate::call_graph::UnresolvedImports::default();
        assert_eq!(super::undeclared_alias_line(&unresolved), None);

        unresolved.aliases.insert("~/queue".to_string(), 9);
        unresolved.aliases.insert("$lib/db".to_string(), 2);
        let line = super::undeclared_alias_line(&unresolved).expect("a line");
        assert_eq!(
            line,
            "Call graph: 11 import(s) through 2 undeclared alias(es) are unresolved: ~/queue, \
             $lib/db. Declare them in tsconfig, package.json or a Deno import map."
        );
    }

    /// The sentence is the deliverable of carrick#1273, so it is pinned.
    ///
    /// A third bucket that still logged a bare count would be the same defect
    /// with better bookkeeping: what a user can act on is the mapping they
    /// wrote, the path it points at, and how many imports went through it.
    #[test]
    fn a_mapping_that_points_at_nothing_is_named_with_its_count() {
        let mut unresolved = crate::call_graph::UnresolvedImports::default();
        unresolved.missing_mappings.insert(
            "@generated-client/".to_string(),
            crate::call_graph::MissingMapping {
                target_root: "src/db/generated/client/".to_string(),
                directory_missing: true,
                imports: 83,
            },
        );
        let lines = super::missing_mapping_lines(&unresolved);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "Call graph: 83 import(s) through `@generated-client/` are unresolved: its target \
             src/db/generated/client/ does not exist. Generate it, or fix the mapping."
        );

        // A file missing beside its siblings is a mis-spelled import, not a
        // build step nobody ran, and the line must not claim otherwise.
        unresolved.missing_mappings.insert(
            "@/*".to_string(),
            crate::call_graph::MissingMapping {
                target_root: "src/absent.ts".to_string(),
                directory_missing: false,
                imports: 1,
            },
        );
        let lines = super::missing_mapping_lines(&unresolved);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("83 import(s)") && lines[1].contains("1 import(s)"),
            "worst first, so the cause leads:\n{lines:#?}"
        );
        assert!(
            lines[1].ends_with("Fix the mapping, or the import."),
            "a file missing beside its siblings is not a directory nobody generated, and the \
             instruction is where that difference reaches a reader:\n{}",
            lines[1]
        );
    }

    /// A blob with only the fields every generation has carried, so the test
    /// states what it is about and nothing else.
    fn bare_blob() -> CloudRepoData {
        let mut blob: CloudRepoData = serde_json::from_value(serde_json::json!({
            "repo_name": "api",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-11T00:00:00Z",
            "commit_hash": "4f2a1c9000000000000000000000000000000000"
        }))
        .expect("a bare blob deserializes");
        blob.file_results = Some(HashMap::from([(
            "src/routes/orders.ts".to_string(),
            crate::agents::file_analyzer_agent::FileAnalysisResult::default(),
        )]));
        blob
    }

    /// A second answer beside `bare_blob`'s, for the file the tests edit.
    fn blob_with_an_edited_file() -> CloudRepoData {
        let mut blob = bare_blob();
        blob.file_results.as_mut().unwrap().insert(
            "src/routes/edited.ts".to_string(),
            crate::agents::file_analyzer_agent::FileAnalysisResult::default(),
        );
        blob
    }

    /// The dirty-tree ruling is warn, never refuse — so the payload is still
    /// uploaded, with the flag beside the commit it qualifies. The cache keeps
    /// the answer about the untouched file and drops the one about the edited
    /// file, whose answer describes bytes no commit holds (carrick#1079).
    #[test]
    fn a_dirty_tree_says_so_and_keeps_only_the_answers_about_committed_bytes() {
        let unchanged = HashSet::from(["src/routes/orders.ts".to_string()]);
        let dirty = stamp_tree_state(blob_with_an_edited_file(), true, Some(&unchanged));
        assert_eq!(dirty.dirty, Some(true));
        let kept: Vec<_> = dirty.file_results.as_ref().unwrap().keys().collect();
        assert_eq!(kept, vec!["src/routes/orders.ts"]);
        // The commit is untouched: `dirty` qualifies it, it does not replace
        // it, and the cloud keys the row on it.
        assert_eq!(
            dirty.commit_hash,
            "4f2a1c9000000000000000000000000000000000"
        );
        let wire = serde_json::to_value(&dirty).unwrap();
        assert_eq!(wire["dirty"], true);
    }

    /// When no answer survives, the cache is omitted rather than sent empty,
    /// and when git could not say which answers describe the commit, a dirty
    /// tree keeps none of them.
    #[test]
    fn a_dirty_tree_git_cannot_describe_keeps_no_answers() {
        let nothing = HashSet::new();
        for dirty in [
            stamp_tree_state(blob_with_an_edited_file(), true, Some(&nothing)),
            stamp_tree_state(blob_with_an_edited_file(), true, None),
        ] {
            assert!(dirty.file_results.is_none());
            let wire = serde_json::to_value(&dirty).unwrap();
            assert!(
                wire.get("file_results").is_none(),
                "the cache is omitted, not sent as null: {wire}"
            );
        }
    }

    /// A clean tree is what CI always has, and its payload must be exactly
    /// what it was before the field existed: no key at all, and the analysis
    /// cache intact so the next run replays it.
    #[test]
    fn a_clean_tree_sends_no_dirty_key_and_keeps_its_cache() {
        let everything = HashSet::from([
            "src/routes/orders.ts".to_string(),
            "src/routes/edited.ts".to_string(),
        ]);
        for clean in [
            stamp_tree_state(blob_with_an_edited_file(), false, Some(&everything)),
            stamp_tree_state(blob_with_an_edited_file(), false, None),
        ] {
            assert_eq!(clean.dirty, None);
            assert_eq!(clean.file_results.as_ref().map(HashMap::len), Some(2));
            let wire = serde_json::to_value(&clean).unwrap();
            assert!(
                wire.get("dirty").is_none(),
                "a clean payload is byte-identical to a pre-field one: {wire}"
            );
        }
    }

    /// A blob written before the field existed reads as clean, which is what
    /// it was. `null` reads the same way, for a writer that spells it out.
    #[test]
    fn an_older_blob_reads_as_clean() {
        assert_eq!(bare_blob().dirty, None);
        let explicit: CloudRepoData = serde_json::from_value(serde_json::json!({
            "repo_name": "api",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-11T00:00:00Z",
            "commit_hash": "abc123",
            "dirty": null
        }))
        .expect("an explicit null deserializes");
        assert_eq!(explicit.dirty, None);
    }
    use super::*;
    use crate::analyzer::ApiEndpointDetails;
    use crate::type_manifest::MISSING_ALIAS_MARKER;

    /// A run that asks for a local cache directory and suppresses every write
    /// into it is told so, in terms that name both variables and the directory
    /// (carrick#966). The pass this catches exits 0 with a full report and an
    /// empty directory behind it.
    #[test]
    fn a_suppressed_local_upload_names_both_variables_and_the_directory() {
        let warning = suppressed_upload_warning(true, Some("/tmp/xrepo-cache"))
            .expect("both set: the directory stays empty and the run must say so");
        assert!(warning.contains("CARRICK_OUTPUT_JSON"), "{warning}");
        assert!(warning.contains("CARRICK_LOCAL_STORAGE_DIR"), "{warning}");
        assert!(warning.contains("/tmp/xrepo-cache"), "{warning}");
    }

    /// A capture pass — a local cache directory and no JSON output — writes its
    /// blob, so there is nothing to say.
    #[test]
    fn a_local_cache_run_that_uploads_is_not_warned() {
        assert_eq!(
            suppressed_upload_warning(false, Some("/tmp/xrepo-cache")),
            None
        );
    }

    /// An eval run with no local cache directory is the ordinary read-only
    /// case: no directory is named, so none is left empty.
    #[test]
    fn json_output_without_a_local_cache_directory_is_not_warned() {
        assert_eq!(suppressed_upload_warning(true, None), None);
    }

    /// The incremental upload path stamps the running release, same as the
    /// full-analysis path. Miss it here and every incremental scan — the common
    /// case — uploads an unattributed blob the cloud can never re-index.
    #[test]
    fn sidecar_service_scope_covers_capture_shared_includes_and_return_to_root() {
        use crate::services::type_sidecar::SymbolRequest;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("api")).unwrap();
        std::fs::create_dir(root.join("shared")).unwrap();
        std::fs::write(
            root.join("api/types.ts"),
            "export interface Local { id: string }",
        )
        .unwrap();
        std::fs::write(
            root.join("shared/types.ts"),
            "export interface Shared { id: string }",
        )
        .unwrap();
        let log = root.join("requests.jsonl");
        let fake = root.join("sidecar.cjs");
        std::fs::write(&fake, format!(r#"
          const fs = require('fs');
          require('readline').createInterface({{input:process.stdin}}).on('line', line => {{
            const request = JSON.parse(line);
            fs.appendFileSync({}, line + '\n');
            if (request.action === 'shutdown') process.exit(0);
            console.log(JSON.stringify({{request_id:request.request_id,
              status:request.action === 'init' ? 'ready' : 'error', errors:['fixture capture stop']}}));
          }});
        "#, serde_json::to_string(&log).unwrap())).unwrap();
        let sidecar = TypeSidecar::spawn(&fake).unwrap();
        std::fs::write(root.join("api/tsconfig.json"), "{}").unwrap();
        let member = Config {
            directory: Some("api".into()),
            tsconfig: Some("tsconfig.json".into()),
            ..Config::default()
        };
        scope_sidecar_to_service(Some(&sidecar), root.to_str().unwrap(), &member);
        let explicit: Vec<_> = [("Local", "api/types.ts"), ("Shared", "shared/types.ts")]
            .into_iter()
            .map(|(name, file)| SymbolRequest {
                symbol_name: name.into(),
                source_file: file.into(),
                alias: Some(name.into()),
                array_depth: None,
                payload_borrow_witness: false,
            })
            .collect();
        let mut data = service_data("fixture", Some("api"));
        let resolution = TypeResolutionResult {
            dts_content: None,
            explicit_manifest: vec![],
            inferred_types: vec![],
            symbol_failures: vec![],
            errors: vec![],
            anchor_changes: vec![],
        };
        run_capture_for_service(
            &sidecar,
            &FileOrchestrator::new(crate::agent_service::AgentService::new()),
            &HashMap::new(),
            root.to_str().unwrap(),
            &MountGraph::default(),
            &member,
            &service_module_index(root.to_str().unwrap(), &member),
            &explicit,
            &[],
            &resolution,
            &mut data,
        );
        scope_sidecar_to_service(Some(&sidecar), root.to_str().unwrap(), &Config::default());
        run_capture_for_service(
            &sidecar,
            &FileOrchestrator::new(crate::agent_service::AgentService::new()),
            &HashMap::new(),
            root.to_str().unwrap(),
            &MountGraph::default(),
            &Config::default(),
            &service_module_index(root.to_str().unwrap(), &Config::default()),
            &explicit,
            &[],
            &resolution,
            &mut data,
        );
        let requests: Vec<serde_json::Value> = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let inits: Vec<_> = requests.iter().filter(|r| r["action"] == "init").collect();
        assert_eq!(
            inits.len(),
            2,
            "root service must reinitialize after a member"
        );
        assert_eq!(
            Path::new(inits[0]["repo_root"].as_str().unwrap()),
            root.join("api")
        );
        assert_eq!(Path::new(inits[1]["repo_root"].as_str().unwrap()), root);
        let capture = requests
            .iter()
            .find(|r| r["action"] == "capture_v2")
            .expect("capture request");
        assert_eq!(
            Path::new(capture["repo_root"].as_str().unwrap()),
            root.join("api")
        );
        assert_eq!(
            capture["tsconfig_path"], "tsconfig.json",
            "capture must preserve the explicit compiler configuration used by init"
        );
        assert_eq!(inits[0]["tsconfig_path"], capture["tsconfig_path"]);
        for anchor in capture["anchors"].as_array().unwrap() {
            let source = root
                .join("api")
                .join(anchor["source_file"].as_str().unwrap())
                .canonicalize()
                .unwrap();
            let expected = if anchor["alias"] == "Local" {
                root.join("api/types.ts")
            } else {
                root.join("shared/types.ts")
            };
            assert_eq!(
                source, expected,
                "capture must preserve source identity across service boundary"
            );
        }
        assert_eq!(capture["anchors"].as_array().unwrap().len(), 2);
        let root_capture = requests
            .iter()
            .rev()
            .find(|r| r["action"] == "capture_v2")
            .unwrap();
        assert_eq!(Path::new(root_capture["repo_root"].as_str().unwrap()), root);
        assert!(
            root_capture.get("tsconfig_path").is_none(),
            "root capture must not retain the previous service's config"
        );
        assert!(
            root_capture["anchors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|anchor| Path::new(anchor["source_file"].as_str().unwrap()).is_relative()),
            "a trailing /. on the capture root must not leak absolute anchors and trigger literal fallback"
        );
    }

    #[test]
    fn npm_manifest_hash_preserves_existing_cache_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let packages = Packages::new(vec![dir.path().join("package.json")]).unwrap();
        assert_eq!(
            hash_workspace_package_jsons(&packages, dir.path().to_str().unwrap()).unwrap(),
            hash_file_content("package.json\0{}\0")
        );
    }

    #[test]
    fn deno_resolution_inputs_invalidate_service_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("api")).unwrap();
        std::fs::create_dir(root.join("shared")).unwrap();
        std::fs::write(
            root.join("deno.jsonc"),
            r#"{
          // inherited mapping and nondefault lock
          "workspace":["./api","./shared"], "importMap":"./imports.json", "lock":"./deps.lock"
        }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("imports.json"),
            r#"{"imports":{"client":"npm:client-lib@1"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("api/deno.json"), r#"{"name":"@sample/api"}"#).unwrap();
        std::fs::write(
            root.join("shared/deno.json"),
            r#"{"name":"@sample/shared","exports":"./v1.ts"}"#,
        )
        .unwrap();
        let service = Config {
            directory: Some("api".into()),
            ..Config::default()
        };
        let packages = load_packages_for_service(root.to_str().unwrap(), &service).unwrap();
        assert!(
            packages
                .package_jsons
                .iter()
                .any(|p| p.dependencies.contains_key("client-lib"))
        );
        let hash = || hash_workspace_package_jsons(&packages, root.to_str().unwrap()).unwrap();
        let before = hash();
        std::fs::write(root.join("deps.lock"), "{}").unwrap();
        let locked = hash();
        assert_ne!(before, locked, "new lock must invalidate");
        std::fs::write(
            root.join("imports.json"),
            r#"{"imports":{"client":"npm:client-lib@2"}}"#,
        )
        .unwrap();
        let mapped = hash();
        assert_ne!(locked, mapped, "external map must invalidate");
        std::fs::write(
            root.join("shared/deno.json"),
            r#"{"name":"@sample/shared","exports":"./v2.ts"}"#,
        )
        .unwrap();
        assert_ne!(mapped, hash(), "sibling export must invalidate");
        std::fs::remove_file(root.join("deps.lock")).unwrap();
        assert_ne!(mapped, hash());
    }

    /// A run's file is read whole up to its last 5 MB, and an empty one reads
    /// nothing rather than underflowing into an allocation abort (carrick#936).
    #[test]
    fn a_run_log_ships_whole_up_to_its_last_five_megabytes() {
        assert_eq!(super::log_tail_range(0), (0, 0));
        assert_eq!(super::log_tail_range(10_000), (0, 10_000));
        let six_mb = 6 * 1024 * 1024;
        let five_mb = 5 * 1024 * 1024;
        assert_eq!(
            super::log_tail_range(six_mb),
            (six_mb - five_mb, five_mb as usize)
        );
    }

    /// The fail marker's `reason` is the error's own words, with the home
    /// directory gone and the whole thing inside the wire's limit
    /// (carrick#1063).
    #[test]
    fn a_fail_reason_is_redacted_and_bounded() {
        let short = super::fail_reason(
            "Carrick Cloud did not open this scan: already running",
            &crate::logging::Redaction::default(),
        );
        assert_eq!(
            short,
            "Carrick Cloud did not open this scan: already running"
        );

        let long = super::fail_reason(&"e".repeat(2_000), &crate::logging::Redaction::default());
        assert_eq!(long.chars().count(), super::FAIL_REASON_LIMIT);
        assert!(long.ends_with("..."), "a cut reason says it was cut");

        // Cut on a character boundary, not a byte one: a run that is already
        // failing must not end in a panic on the way out.
        let multibyte =
            super::fail_reason(&"é".repeat(2_000), &crate::logging::Redaction::default());
        assert_eq!(multibyte.chars().count(), super::FAIL_REASON_LIMIT);
    }

    /// A run that dies names the stage it died in, and the marker carries the
    /// error's own words (carrick#1063).
    ///
    /// Driven through the same function the engine calls on its error path,
    /// with the process-global stage set as the pipeline would have left it.
    #[tokio::test]
    async fn a_failing_run_marks_the_stage_it_died_in() {
        let storage = crate::cloud_storage::MockStorage::new();
        crate::scan_stage::enter(crate::scan_stage::Stage::FileAnalysis);

        let error: Box<dyn std::error::Error> =
            "Failed to download cross-repo data: connection reset".into();
        super::report_scan_failure(&storage, ".", error.as_ref()).await;

        assert_eq!(
            storage.scan_failures(),
            vec![(
                "file_analysis".to_string(),
                "Failed to download cross-repo data: connection reset".to_string()
            )]
        );
        crate::scan_stage::enter(crate::scan_stage::Stage::Unknown);
    }

    /// A laptop run that stops because Deno is missing reports it once, before
    /// any scan exists, naming the stage and a reason with the home directory
    /// taken out (carrick#1096).
    ///
    /// The error is the real one `require_runtime` raises for a Deno service
    /// with no runtime answering, and the service lives under a temporary
    /// home so the path in it is one the redaction must rewrite. `#[serial]`
    /// because `HOME` is process-wide.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_run_stopped_by_a_missing_runtime_reports_it_before_any_scan() {
        let home = tempfile::tempdir().expect("temp home");
        let repo = home.path().join("work").join("api");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::write(repo.join("deno.json"), "{}").expect("deno manifest");
        let services = vec![crate::config::Config::default()];
        let error: Box<dyn std::error::Error> =
            crate::deno_support::require_runtime_with(&repo, &services, || None)
                .expect_err("a Deno service with no runtime is refused")
                .into();
        assert!(
            error
                .to_string()
                .contains(&home.path().display().to_string()),
            "the unredacted error names the home: {error}"
        );

        let previous = std::env::var_os("HOME");
        // SAFETY: a `#[serial]` test, and the variable is restored below.
        unsafe { std::env::set_var("HOME", home.path()) };
        let storage = crate::cloud_storage::MockStorage::new();
        super::report_preflight_failure(
            &storage,
            &repo.display().to_string(),
            crate::scan_stage::Stage::Preflight,
            error.as_ref(),
        )
        .await;
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }

        let reported = storage.preflight_failures();
        assert_eq!(reported.len(), 1, "exactly one event: {reported:?}");
        let (repo_name, stage, reason) = &reported[0];
        assert_eq!(repo_name, &None, "a checkout with no origin names no repo");
        assert_eq!(stage, "preflight");
        // The service sits at the root of the repo being scanned, which the
        // redaction names `<repo>` (carrick#1098), so neither the home nor
        // the checkout's own path is in the reason.
        assert!(
            reason.starts_with("Deno is required to scan <repo>/")
                && reason.contains("deno.json. Install or upgrade to Deno"),
            "{reason}"
        );
        assert!(
            !reason.contains(&home.path().display().to_string()),
            "{reason}"
        );
        assert!(reason.chars().count() <= super::FAIL_REASON_LIMIT);
        assert!(
            storage.scan_failures().is_empty(),
            "no scan was opened, so nothing was marked failed"
        );
    }

    /// A reason line naming a credential goes whole, as it would from the run
    /// log; a reason that was nothing but such lines still says the run died.
    #[test]
    fn a_fail_reason_never_carries_a_credential_line() {
        let mixed = super::fail_reason(
            "start-scan refused\nAuthorization: Bearer carrick_sk_x",
            &crate::logging::Redaction::default(),
        );
        assert_eq!(mixed, "start-scan refused");

        let only = super::fail_reason(
            "token carrick_sk_live_abc was rejected",
            &crate::logging::Redaction::default(),
        );
        assert!(!only.contains("carrick_sk_"), "{only}");
        assert!(only.contains("named a credential"), "{only}");
    }

    /// What leaves the machine is redacted, and the wiring that does it is
    /// the upload path itself (carrick#1063).
    ///
    /// Drives the real upload against a temporary home, so the redaction it
    /// applies is the one a laptop run uses. `#[serial]` because `HOME` is
    /// process-wide.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_uploaded_run_log_carries_no_home_and_no_credential() {
        let home = tempfile::tempdir().expect("temp home");
        // A checkout outside the home directory (carrick#1098).
        let repo = tempfile::tempdir().expect("temp repo");
        let runs = home.path().join(".carrick").join("logs").join("runs");
        std::fs::create_dir_all(&runs).expect("log dir");
        let own = runs.join("2026-09-15T13-28-04Z-e52ff358-41234.log");
        std::fs::write(
            &own,
            format!(
                "reading {}/work/api/src/index.ts\n  Authorization: Bearer secret\nanalysed 12 files\nparsing {}/src/a.ts\n",
                home.path().display(),
                repo.path().display()
            ),
        )
        .expect("seed log");

        let previous = std::env::var_os("HOME");
        // SAFETY: a `#[serial]` test, and the variable is restored below.
        unsafe { std::env::set_var("HOME", home.path()) };
        let storage = crate::cloud_storage::MockStorage::new();
        upload_run_log_from(&storage, &repo.path().to_string_lossy(), Some(&own)).await;
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }

        let uploaded = storage.uploaded_logs();
        assert_eq!(uploaded.len(), 1, "nothing was uploaded");
        let log = &uploaded[0];
        assert!(log.contains("reading ~/work/api/src/index.ts"), "{log}");
        assert!(log.contains("analysed 12 files"), "{log}");
        assert!(!log.contains("Authorization"), "{log}");
        assert!(!log.contains(&home.path().display().to_string()), "{log}");
        assert!(log.contains("parsing <repo>/src/a.ts"), "{log}");
    }

    /// The field report behind carrick#1133: while one scan ran, every other
    /// carrick process on the machine wrote the same daily file, and the
    /// upload shipped their lines as this scan's. The upload reads this run's
    /// own file, so neither another run's file nor the daily file reaches it.
    #[tokio::test]
    async fn a_run_log_upload_holds_this_runs_lines_and_no_other_process() {
        let logs = tempfile::tempdir().expect("log dir");
        let runs = logs.path().join("runs");
        std::fs::create_dir_all(&runs).expect("runs dir");
        let banner =
            |run: &str| format!("INFO carrick::logging: Carrick run starting run_id=\"{run}\"\n");
        let own = runs.join("2026-09-15T13-28-04Z-e52ff358-41234.log");
        std::fs::write(
            &own,
            format!("{}analysed 12 files\n", banner("e52ff358-own")),
        )
        .unwrap();
        std::fs::write(
            runs.join("2026-09-15T13-28-05Z-0badc0de-41300.log"),
            banner("0badc0de-other"),
        )
        .unwrap();
        std::fs::write(
            logs.path().join("carrick.log.2026-09-15"),
            format!("{}{}", banner("e52ff358-own"), banner("feedface-lsp")),
        )
        .unwrap();

        let storage = crate::cloud_storage::MockStorage::new();
        upload_run_log_from(&storage, "/repos/api", Some(&own)).await;

        let uploaded = storage.uploaded_logs();
        assert_eq!(uploaded.len(), 1, "nothing was uploaded");
        assert_eq!(
            uploaded[0].matches("Carrick run starting").count(),
            1,
            "{}",
            uploaded[0]
        );
        assert!(uploaded[0].contains("e52ff358-own"), "{}", uploaded[0]);
        assert!(uploaded[0].contains("analysed 12 files"), "{}", uploaded[0]);
        for foreign in ["0badc0de-other", "feedface-lsp"] {
            assert!(!uploaded[0].contains(foreign), "{foreign}: {}", uploaded[0]);
        }

        // A process with no file of its own uploads nothing, rather than
        // falling back to a file other processes write.
        let none = crate::cloud_storage::MockStorage::new();
        upload_run_log_from(&none, "/repos/api", None).await;
        assert!(none.uploaded_logs().is_empty());
    }

    /// A run refused at `start-scan` never opened a scan, and a laptop run's
    /// log is stored against its scan. With no id to name, the only answer the
    /// cloud can give is `404 scan_not_started`: a warn line there and a
    /// wasted request here, from a run that is already being told to stop
    /// (carrick#1370).
    ///
    /// What makes an absence provable is the probe at the end. The stub has
    /// exactly one answer left after the refusal, so a log upload that went
    /// out would take it and the probe would find nobody listening.
    #[tokio::test]
    async fn a_run_refused_at_start_scan_ships_no_log_for_the_scan_it_never_opened() {
        let (base, server) = crate::agent_service::tests::stub_server(vec![
            (
                409,
                serde_json::json!({
                    "error": "A scan of example/api is already running.",
                    "code": "laptop_scan_in_flight"
                })
                .to_string(),
            ),
            (200, serde_json::json!({ "ok": true }).to_string()),
        ]);
        let url = format!("{base}/types/check-or-upload");
        let storage = crate::cloud_storage::AwsStorage::for_test(
            &url,
            crate::credentials::CloudAuth::Bearer("carrick_sk_live_test".to_string()),
            false,
        );
        let refusal = storage
            .begin_run(&crate::cloud_storage::RunContext {
                repo_full_name: Some("example/api".to_string()),
                commit: "4f2a1c9000000000000000000000000000000000".to_string(),
                dirty: false,
            })
            .await
            .expect_err("the slot is held by the scan that is still in flight");
        assert!(refusal.to_string().contains("laptop_scan_in_flight"));

        let logs = tempfile::tempdir().expect("log dir");
        let own = logs.path().join("2026-09-21T09-00-00Z-e52ff358-41234.log");
        std::fs::write(&own, "analysed 12 files\n").expect("seed log");
        upload_run_log_from(&storage, "/repos/api", Some(&own)).await;

        let probe = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("a client")
            .post(&url)
            .json(&serde_json::json!({ "action": "carrick-1370-probe" }))
            .send()
            .await
            .expect("the stub still had its second answer for a request of this test's own");
        assert!(probe.status().is_success());

        let seen = server.join().expect("the stub server thread");
        assert_eq!(seen.len(), 2, "{seen:#?}");
        assert!(
            !seen.iter().any(|request| request.contains("upload-logs")),
            "a run with no scan open sent a log anyway:\n{seen:#?}"
        );
        assert!(seen[1].contains("carrick-1370-probe"), "{seen:#?}");
    }

    /// A refusal that is simply a cloud without the action deployed is not a
    /// warning on the way out of every scan; a real failure still is.
    #[test]
    fn only_a_403_or_404_reads_as_a_cloud_that_cannot_take_the_log() {
        assert!(super::cloud_has_not_deployed_log_upload(
            "Lambda returned error 403: {\"message\":\"Forbidden\"}"
        ));
        assert!(super::cloud_has_not_deployed_log_upload(
            "Lambda returned error 404: Not Found"
        ));
        assert!(!super::cloud_has_not_deployed_log_upload(
            "Lambda returned error 500: internal error"
        ));
        assert!(!super::cloud_has_not_deployed_log_upload(
            "Lambda request failed: connection reset"
        ));
        // Not a status that happens to appear inside a longer number.
        assert!(!super::cloud_has_not_deployed_log_upload(
            "Lambda returned error 500: object 4030 is missing"
        ));
    }

    #[test]
    fn incremental_cloud_data_stamps_the_running_scanner_version() {
        let data = build_cloud_data_from_mount_graph(
            "orders-service",
            ".",
            &MountGraph::new(),
            &Config::default(),
            &crate::packages::Packages::default(),
            HashMap::new(),
        );

        assert_eq!(
            data.scanner_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            data.scanner_build,
            crate::cloud_storage::ScannerBuild::current()
        );
    }

    /// "Uploaded" would be a lie when the cloud short-circuited every payload,
    /// and "already current" would be a lie when it re-indexed any of them.
    #[test]
    fn upload_finish_message_only_claims_current_when_every_payload_was_skipped() {
        let skipped = UploadOutcome {
            already_current: true,
            ..UploadOutcome::default()
        };
        let indexed = UploadOutcome::default();

        assert_eq!(
            upload_finish_message(&[skipped.clone(), skipped.clone()]),
            "Index already current for this commit and scanner version; nothing re-indexed"
        );
        // A multi-service repo where one service did get re-indexed uploaded
        // something, so it must not claim otherwise.
        assert_eq!(
            upload_finish_message(&[skipped, indexed.clone()]),
            "Uploaded results to Carrick Cloud"
        );
        assert_eq!(
            upload_finish_message(&[indexed]),
            "Uploaded results to Carrick Cloud"
        );
        // Vacuously "all skipped" — but nothing was uploaded to call current.
        assert_eq!(
            upload_finish_message(&[]),
            "Uploaded results to Carrick Cloud"
        );
    }

    /// carrick#885: a forced run that gets "already current" back paid for a
    /// re-analysis the cloud discarded, and the only way that happens is a
    /// cloud deployed without the `force_reindex` reader. Say so instead of
    /// printing the ordinary line.
    #[test]
    fn a_forced_run_that_was_short_circuited_is_a_warning_not_a_status_line() {
        let skipped = UploadOutcome {
            already_current: true,
            ..UploadOutcome::default()
        };
        let indexed = UploadOutcome::default();

        assert!(forced_reanalysis_was_discarded(
            &[skipped.clone(), skipped.clone()],
            true
        ));

        // A cached run that finds the index current is the ordinary case the
        // short-circuit exists for, and says nothing about a deploy.
        assert!(!forced_reanalysis_was_discarded(
            &[skipped.clone(), skipped.clone()],
            false
        ));

        // Any service that was re-indexed means the cloud honoured the flag.
        assert!(!forced_reanalysis_was_discarded(
            &[skipped, indexed.clone()],
            true
        ));
        assert!(!forced_reanalysis_was_discarded(&[indexed], true));

        // Nothing was uploaded, so nothing was discarded.
        assert!(!forced_reanalysis_was_discarded(&[], true));
    }

    /// A blank service payload, named, with nothing resolved.
    fn service_data(repo: &str, service: Option<&str>) -> CloudRepoData {
        CloudRepoData {
            repo_name: repo.to_string(),
            service_name: service.map(str::to_string),
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: String::new(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        }
    }

    /// A storage whose every answer is scripted, so the upload loop's
    /// decisions can be driven without a cloud: what each upload returns, what
    /// the landed-check says on each successive ask, and what was asked.
    struct ScriptedStorage {
        uploads: std::sync::Mutex<std::collections::VecDeque<Result<UploadOutcome, StorageError>>>,
        landed: std::sync::Mutex<std::collections::VecDeque<Result<bool, StorageError>>>,
        uploaded: std::sync::Mutex<Vec<String>>,
        /// The serialized body of each upload, as the wire would carry it.
        uploaded_bodies: std::sync::Mutex<Vec<String>>,
        landed_asks: std::sync::Mutex<Vec<String>>,
        /// Whether the upload loop told this storage the run had analysed
        /// files, and so that its write actions must supersede the stored
        /// generation (carrick#1306).
        analyzed_noted: std::sync::atomic::AtomicBool,
    }

    impl ScriptedStorage {
        fn new(
            uploads: Vec<Result<UploadOutcome, StorageError>>,
            landed: Vec<Result<bool, StorageError>>,
        ) -> Self {
            Self {
                uploads: std::sync::Mutex::new(uploads.into()),
                landed: std::sync::Mutex::new(landed.into()),
                uploaded: std::sync::Mutex::new(Vec::new()),
                uploaded_bodies: std::sync::Mutex::new(Vec::new()),
                landed_asks: std::sync::Mutex::new(Vec::new()),
                analyzed_noted: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn analyzed_noted(&self) -> bool {
            self.analyzed_noted
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        fn uploaded(&self) -> Vec<String> {
            self.uploaded.lock().unwrap().clone()
        }

        fn uploaded_bodies(&self) -> Vec<String> {
            self.uploaded_bodies.lock().unwrap().clone()
        }

        fn landed_asks(&self) -> Vec<String> {
            self.landed_asks.lock().unwrap().clone()
        }
    }

    fn scripted_service(data: &CloudRepoData) -> String {
        data.service_name
            .clone()
            .unwrap_or_else(|| data.repo_name.clone())
    }

    #[async_trait::async_trait]
    impl CloudStorage for ScriptedStorage {
        fn note_analyzed_files(&self) {
            self.analyzed_noted
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        async fn upload_repo_data(
            &self,
            data: &CloudRepoData,
            _final_in_run: bool,
        ) -> Result<UploadOutcome, StorageError> {
            self.uploaded.lock().unwrap().push(scripted_service(data));
            self.uploaded_bodies
                .lock()
                .unwrap()
                .push(serde_json::to_string(data).expect("payload serializes"));
            self.uploads
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted upload outcome")
        }

        async fn index_landed(
            &self,
            data: &CloudRepoData,
            _written_after: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool, StorageError> {
            self.landed_asks
                .lock()
                .unwrap()
                .push(scripted_service(data));
            self.landed.lock().unwrap().pop_front().unwrap_or(Ok(false))
        }

        async fn download_all_repo_data(
            &self,
        ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
            Ok((Vec::new(), HashMap::new()))
        }

        async fn upload_type_file(
            &self,
            _repo_name: &str,
            _file_name: &str,
            _content: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<(), StorageError> {
            Ok(())
        }

        async fn upload_logs(&self, _repo: &str, _log: &str) -> Result<(), StorageError> {
            Ok(())
        }

        async fn post_pr_result(
            &self,
            _payload: &crate::findings::PrResultPayload,
        ) -> Result<(), StorageError> {
            Ok(())
        }
    }

    fn lost_write(handler_may_still_run: bool) -> StorageError {
        StorageError::UncertainWrite(crate::cloud_storage::UncertainWrite {
            action: "complete-upload".to_string(),
            handler_may_still_run,
            message: "Lambda returned 400: no staged payload".to_string(),
        })
    }

    /// A boundary with no machine prefixes: the upload tests that are about
    /// delivery, not paths.
    fn no_boundary() -> upload_boundary::UploadBoundary {
        upload_boundary::UploadBoundary::new("", None)
    }

    /// carrick#1204. A stub file an upstream pass left holding the checkout
    /// root and the home directory goes up with placeholders, the upload is
    /// not refused, and the payload the run keeps in memory is untouched.
    #[tokio::test]
    async fn the_upload_boundary_scrubs_a_machine_path_an_upstream_pass_missed() {
        let root = "/home/user/work/acme-app";
        let home = "/home/user";
        let mut leaking = service_data("acme-app", Some("orders"));
        leaking.capture_stub = Some(crate::cloud_storage::CaptureStubArtifact {
            artifact_version: 1,
            package_name: "@carrick/orders".to_string(),
            ts_version: "5.8.2".to_string(),
            bare_checkout: false,
            files: std::collections::BTreeMap::from([(
                "types/surface.d.ts".to_string(),
                format!(
                    "export type A = import(\"{root}/node_modules/.store/kit@1.0.0/node_modules/kit/index\").T;\n\
                     export type B = import(\"{home}/.cache/runtime/npm/registry.example.org/kit/1.0.0/index\").T;\n"
                ),
            )]),
        });
        let payloads = vec![leaking];
        let storage = ScriptedStorage::new(vec![Ok(UploadOutcome::default())], vec![]);

        let unconfirmed = upload_service_payloads(
            &storage,
            &payloads,
            false,
            true,
            &upload_boundary::UploadBoundary::new(root, Some(home)),
        )
        .await;

        assert!(unconfirmed.is_empty(), "{unconfirmed:?}");
        let bodies = storage.uploaded_bodies();
        assert_eq!(bodies.len(), 1, "the upload is never refused");
        assert!(
            !bodies[0].contains(home),
            "uploaded body holds a machine path: {}",
            bodies[0]
        );
        assert!(bodies[0].contains("<checkout>/node_modules/.store/kit@1.0.0"));
        assert!(bodies[0].contains("<home>/.cache/runtime/npm"));
        let kept = &payloads[0].capture_stub.as_ref().unwrap().files["types/surface.d.ts"];
        assert!(
            kept.contains(root),
            "the in-memory payload is not rewritten"
        );
    }

    fn two_services() -> Vec<CloudRepoData> {
        vec![
            service_data("api-server", Some("orders")),
            service_data("api-server", Some("billing")),
        ]
    }

    /// A payload whose boundary says this scan sent `files` to the analyzer.
    fn analyzed_service(service: &str, files: usize) -> CloudRepoData {
        let mut data = service_data("api-server", Some(service));
        data.boundary = Some(crate::boundary::ServiceBoundary {
            files_attempted: files,
            ..Default::default()
        });
        data
    }

    /// carrick#1306. The incident: a user did what the pre-flight refusal told
    /// them to, the generated output was gitignored so the tree stayed clean,
    /// and the rescan ran at the same commit without `--no-cache`. Nothing the
    /// cloud's freshness guard reads had moved, so it kept the stored
    /// generation and the scan's fresh answers were never served. The run
    /// analysed files, and this is where it has to say so — before the first
    /// write, since every write action reads the flag.
    #[tokio::test]
    async fn a_run_that_analyzed_files_tells_the_storage_before_it_uploads() {
        let storage = ScriptedStorage::new(
            vec![Ok(UploadOutcome::default()), Ok(UploadOutcome::default())],
            vec![],
        );

        let unconfirmed = upload_service_payloads(
            &storage,
            &[
                analyzed_service("orders", 0),
                analyzed_service("billing", 3),
            ],
            false,
            true,
            &no_boundary(),
        )
        .await;

        assert!(unconfirmed.is_empty());
        assert!(
            storage.analyzed_noted(),
            "one analysed service is enough: its fresh answers reach the others through the \
             cross-repo join, so the whole run's generation differs"
        );
    }

    /// And the other half of the acceptance: a rescan that analysed nothing
    /// says nothing, so an unchanged repo is still deduped and re-ingests
    /// nothing.
    #[tokio::test]
    async fn a_run_that_analyzed_nothing_leaves_the_dedupe_alone() {
        let storage = ScriptedStorage::new(
            vec![Ok(UploadOutcome::default()), Ok(UploadOutcome::default())],
            vec![],
        );

        upload_service_payloads(
            &storage,
            &[
                analyzed_service("orders", 0),
                analyzed_service("billing", 0),
            ],
            false,
            true,
            &no_boundary(),
        )
        .await;

        assert!(!storage.analyzed_noted());
    }

    /// carrick#1067. The incident: the write landed and its response did not,
    /// so the cloud holds this commit. The run says nothing failed and goes on
    /// to the next service, which is the whole point — the remaining services
    /// of a thirteen-service repo used to be abandoned here.
    #[tokio::test]
    async fn a_lost_write_that_landed_is_not_a_failure() {
        let storage = ScriptedStorage::new(
            vec![Err(lost_write(false)), Ok(UploadOutcome::default())],
            vec![Ok(true)],
        );

        let unconfirmed =
            upload_service_payloads(&storage, &two_services(), false, true, &no_boundary()).await;

        assert!(unconfirmed.is_empty(), "{unconfirmed:?}");
        assert_eq!(storage.uploaded(), vec!["orders", "billing"]);
        assert_eq!(storage.landed_asks(), vec!["orders"]);
    }

    /// A write that really did not land is reported — and the service after it
    /// is still uploaded, so one failure costs one service's index rather than
    /// the rest of the run.
    #[tokio::test]
    async fn a_write_that_did_not_land_is_reported_and_the_run_continues() {
        let storage = ScriptedStorage::new(
            vec![Err(lost_write(false)), Ok(UploadOutcome::default())],
            vec![Ok(false)],
        );

        let unconfirmed =
            upload_service_payloads(&storage, &two_services(), false, true, &no_boundary()).await;

        assert_eq!(unconfirmed.len(), 1);
        assert_eq!(unconfirmed[0].service, "orders");
        assert!(unconfirmed[0].reason.contains("complete-upload"));
        assert_eq!(storage.uploaded(), vec!["orders", "billing"]);
    }

    /// A refusal the cloud answered is a decision about a write that never
    /// ran. Asking what the index holds would either repeat the refusal or
    /// read the generation this run was replacing, so it is not asked.
    #[tokio::test]
    async fn a_refusal_the_cloud_answered_is_never_re_checked() {
        let storage = ScriptedStorage::new(
            vec![
                Err(StorageError::ConnectionError(
                    "index would lose files (partial_refused, HTTP 409)".to_string(),
                )),
                Ok(UploadOutcome::default()),
            ],
            vec![],
        );

        let unconfirmed =
            upload_service_payloads(&storage, &two_services(), false, true, &no_boundary()).await;

        assert_eq!(unconfirmed.len(), 1);
        assert!(
            storage.landed_asks().is_empty(),
            "nothing was left running to ask about"
        );
        assert_eq!(storage.uploaded(), vec!["orders", "billing"]);
    }

    /// The cloud commits after the gateway has already cut the response — 25 s
    /// after, on the 2026-09-14 incident — so one check a moment later would
    /// report a successful upload as lost and spend one of the day's three
    /// scans re-running it. Time is paused here: the test asserts the asking,
    /// not the waiting.
    #[tokio::test(start_paused = true)]
    async fn a_handler_that_may_still_be_running_is_asked_again() {
        let storage = ScriptedStorage::new(
            vec![Err(lost_write(true)), Ok(UploadOutcome::default())],
            vec![Ok(false), Ok(false), Ok(true)],
        );

        let unconfirmed =
            upload_service_payloads(&storage, &two_services(), false, true, &no_boundary()).await;

        assert!(unconfirmed.is_empty(), "{unconfirmed:?}");
        assert_eq!(storage.landed_asks().len(), 3);
    }

    /// A check that cannot be made answers "not landed": the run reports a
    /// service it could not confirm, which is true, rather than claiming one
    /// it cannot see.
    #[tokio::test]
    async fn a_landed_check_that_fails_leaves_the_service_unconfirmed() {
        let storage = ScriptedStorage::new(
            vec![Err(lost_write(false))],
            vec![Err(StorageError::ConnectionError("no remote".to_string()))],
        );

        let unconfirmed =
            upload_service_payloads(&storage, &two_services()[..1], false, true, &no_boundary())
                .await;

        assert_eq!(unconfirmed.len(), 1);
    }

    /// Which failures are worth asking about, and for how long.
    #[test]
    fn only_a_lost_write_is_checked_and_only_a_live_handler_is_waited_for() {
        assert!(
            landed_check_waits(&StorageError::ConnectionError("409".to_string())).is_none(),
            "an answered refusal is not a lost response"
        );
        assert!(
            landed_check_waits(&lost_write(false))
                .expect("a lost write is checked")
                .is_empty(),
            "nothing is still running, so one ask settles it"
        );
        let waits = landed_check_waits(&lost_write(true)).expect("a lost write is checked");
        assert_eq!(waits.len(), LANDED_CHECK_WAITS_SECONDS.len());
        assert_eq!(
            waits.iter().map(Duration::as_secs).sum::<u64>(),
            LANDED_CHECK_WAITS_SECONDS.iter().sum::<u64>(),
            "the window must cover a handler that commits after the gateway cut"
        );
    }

    /// "Mixed-generation" is about a PARTIAL upload. When nothing landed,
    /// nothing was replaced, and the index is exactly as it was
    /// (carrick#1023 item 4).
    #[test]
    fn the_summary_only_claims_damage_when_some_services_landed() {
        let failed = vec![UnconfirmedUpload {
            service: "orders".to_string(),
            reason: "Lost the response to 'complete-upload'".to_string(),
        }];

        let nothing = unconfirmed_upload_summary(&[], &failed);
        assert!(nothing.contains("Nothing was uploaded"), "{nothing}");
        assert!(nothing.contains("still holds what it held"), "{nothing}");
        assert!(!nothing.contains("mixed-generation"), "{nothing}");

        let partial = unconfirmed_upload_summary(&["billing"], &failed);
        assert!(partial.contains("Uploaded: [billing]"), "{partial}");
        assert!(partial.contains("not uploaded: [orders]"), "{partial}");
        assert!(partial.contains("mixed-generation"), "{partial}");

        // The run's last word names every service and what it said.
        let error = unconfirmed_upload_error(&failed);
        assert!(error.contains("orders"), "{error}");
        assert!(error.contains("complete-upload"), "{error}");
        assert!(error.contains("re-run the scan"), "{error}");
    }

    /// Only a service that actually lost its types produces a finding, and it
    /// is named by the service, not the repo, so a monorepo says which one
    /// (carrick#535).
    #[test]
    fn degraded_type_findings_name_the_service_that_lost_types() {
        let mut degraded = service_data("api-server", Some("orders"));
        degraded.types_degraded = Some(TypeDegradation {
            stage: "resolve".to_string(),
            detail: "I/O error: Broken pipe (os error 32)".to_string(),
        });
        let healthy = service_data("api-server", Some("billing"));

        let findings = degraded_type_findings(&[degraded, healthy]);

        assert_eq!(
            findings,
            vec![crate::findings::Finding::degraded_types(
                "orders",
                "resolve",
                "I/O error: Broken pipe (os error 32)"
            )]
        );
        assert!(degraded_type_findings(&[service_data("api-server", None)]).is_empty());
    }

    #[test]
    fn service_scan_root_joins_declared_directory() {
        let flat = Config::default();
        assert_eq!(
            service_scan_root("/repo", &flat),
            std::path::PathBuf::from("/repo")
        );

        let service = Config {
            directory: Some("apps/web".to_string()),
            ..Config::default()
        };
        assert_eq!(
            service_scan_root("/repo", &service),
            std::path::PathBuf::from("/repo/apps/web")
        );
    }
    use crate::cloud_storage::TypeEvidence;
    use crate::services::type_sidecar::{InferredType, SourceLocation};
    use crate::visitor::{OwnerType, TypeReference};
    use std::path::PathBuf;

    /// Cloud-bound function definitions must carry repo-relative paths: the
    /// extractor stamps the absolute CI checkout path (and the compiler leaks
    /// it into signatures via `import("...")`), which breaks GitHub deep
    /// links and the MCP tools' `gh api .../contents/{file_path}` hint.
    #[test]
    fn cloud_projection_relativizes_function_paths() {
        use crate::visitor::{FunctionCallRef, FunctionDefinition};

        let repo_path = "/home/runner/work/acme/acme";
        let mut defs = HashMap::new();
        defs.insert(
            "handler".to_string(),
            FunctionDefinition {
                name: "handler".to_string(),
                file_path: PathBuf::from("/home/runner/work/acme/acme/src/api/handler.ts"),
                node_type: Default::default(),
                arguments: vec![],
                body_source: None,
                is_exported: true,
                line_number: 3,
                end_line: 0,
                intent: Some("handles the thing".to_string()),
                calls: vec![FunctionCallRef {
                    name: "helper".to_string(),
                    file_path: "/home/runner/work/acme/acme/src/lib/helper.ts".to_string(),
                    line_number: 9,
                    call_site_line: 4,
                    call_count: 1,
                }],
                tokens: vec![],
                return_type: None,
                return_is_explicit: false,
                signature: Some(
                    "(req: import(\"/home/runner/work/acme/acme/src/types\").Req) => void"
                        .to_string(),
                ),
                intent_input_hash: None,
                dispatch_table: None,
            },
        );
        // A path outside the repo root is left as-is (matches the dashboard's
        // read-side posture: never mangle what we can't confidently strip).
        defs.insert(
            "external".to_string(),
            FunctionDefinition {
                name: "external".to_string(),
                file_path: PathBuf::from("/opt/other/place.ts"),
                node_type: Default::default(),
                arguments: vec![],
                body_source: None,
                is_exported: true,
                line_number: 1,
                end_line: 0,
                intent: None,
                calls: vec![],
                tokens: vec![],
                return_type: None,
                return_is_explicit: false,
                signature: None,
                intent_input_hash: None,
                dispatch_table: None,
            },
        );

        relativize_function_definition_paths(
            &mut defs,
            repo_path,
            &served_paths::PathScrub::new(repo_path, None),
        );

        let handler = &defs["handler"];
        assert_eq!(handler.file_path, PathBuf::from("src/api/handler.ts"));
        assert_eq!(handler.calls[0].file_path, "src/lib/helper.ts");
        assert_eq!(
            handler.signature.as_deref(),
            Some("(req: import(\"src/types\").Req) => void"),
        );
        assert_eq!(
            defs["external"].file_path,
            PathBuf::from("/opt/other/place.ts")
        );
    }

    /// Every path a payload carries must be repo-relative before it is
    /// uploaded, whichever analysis branch built it. The full branch used to
    /// project its mount graph from `file_results` keyed by absolute
    /// as-scanned paths, so the same repo shipped absolute call sites on a
    /// full scan and relative ones on an incremental scan.
    ///
    /// Walks the serialized blob for any string that still CONTAINS the repo
    /// root or the home directory, anywhere in it: printed TypeScript embeds
    /// both mid-string (`import("/abs/path").Name`), and a package store or a
    /// runtime cache under home is the account name on a served row
    /// (carrick#1160). The sweep covers every file of the capture stub too.
    /// The pass leaves the stub's declaration files byte-identical, because
    /// they are compiled again at check time (see `relativize_cloud_paths`);
    /// the capture itself writes them without absolute specifiers
    /// (carrick#1174, guarded behaviourally by the sidecar's
    /// `capture-v2-installed-package-specifier` test). This test fails if the
    /// payload's stub carries a path, or if the pass starts editing it.
    #[test]
    fn relativize_cloud_paths_leaves_no_absolute_path_in_the_payload() {
        const STUB_SURFACE: &str = "import type { Order } from \"./src/types/order\";\n\
            export type Endpoint_Response = { order: Order; ctx: import(\"web-kit\").Context };\n";
        use crate::external_call_candidates::{CallMechanism, ExternalCallCandidate};
        use crate::mount_graph::{DataFetchingCall, GraphNode, NodeType, ResolvedEndpoint};
        use crate::packages::PackageInfo;
        use crate::visitor::FunctionDefinition;

        let repo_path = "/home/runner/work/acme-app/acme-app";
        let home = "/home/runner";
        let abs = |rel: &str| format!("{}/{}", repo_path, rel);
        // A runtime's npm cache under the account's home directory.
        let cached =
            |pkg: &str| format!("{home}/.cache/runtime/npm/registry.npmjs.org/{pkg}/dist/types");

        let op = |file: &str| ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::http("GET", "/orders".to_string()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: Some(TypeReference {
                file_path: PathBuf::from(abs("src/types/order.ts")),
                type_ann: None,
                start_position: 0,
                composite_type_string: "Order".to_string(),
                alias: "Order".to_string(),
            }),
            response_type: Some(TypeReference {
                file_path: PathBuf::from(abs("src/types/order.ts")),
                type_ann: None,
                start_position: 0,
                composite_type_string: "Order".to_string(),
                alias: "Order".to_string(),
            }),
            file_path: PathBuf::from(abs(file)),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: None,
            schema_binding: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
        };

        let mut graph = MountGraph::new();
        graph.nodes.insert(
            "app".to_string(),
            GraphNode {
                name: "app".to_string(),
                node_type: NodeType::Root,
                creation_site: Some(abs("src/server.ts:3")),
                file_location: abs("src/server.ts:3"),
            },
        );
        graph.endpoints.push(ResolvedEndpoint {
            view_module: false,
            method: "GET".to_string(),
            path: "/orders".to_string(),
            full_path: "/orders".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: abs("src/routes/orders.ts:18"),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            dispatch: None,
            handler_span: None,
        });
        graph.data_calls.push(DataFetchingCall {
            method: "GET".to_string(),
            target_url: "http://api/search".to_string(),
            canonical_path: "/search".to_string(),
            client: "fetch(".to_string(),
            file_location: abs("src/providers/search.ts:313"),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(313),
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
            role: Some(crate::mount_graph::ConsumerRole::WrapperCall),
            // A second location on the same row (carrick#1402), written by a
            // join that reads the scan's own absolute paths.
            reaches_request: Some(abs("src/lib/search-client.ts:88")),
            library_semantics: Vec::new(),
        });

        let mut function_definitions = HashMap::new();
        function_definitions.insert(
            "handler".to_string(),
            FunctionDefinition {
                name: "handler".to_string(),
                file_path: PathBuf::from(abs("src/routes/orders.ts")),
                node_type: Default::default(),
                body_source: None,
                is_exported: true,
                line_number: 18,
                end_line: 24,
                intent: None,
                calls: vec![],
                tokens: vec![],
                arguments: vec![crate::visitor::FunctionArgument {
                    name: "c".to_string(),
                    type_ann: None,
                    type_string: Some(format!(
                        "import(\"{}\").Ctx",
                        abs("node_modules/.pnpm/@acme+kit@2.0.1_react@18.2.0/node_modules/@acme/kit/dist/index")
                    )),
                    is_explicit: false,
                    is_optional: false,
                    has_default: false,
                    default_value: None,
                    is_rest: false,
                }],
                return_type: Some(format!(
                    "Promise<import(\"{}\").TypedResponse<any>>",
                    cached("web-kit/4.12.12")
                )),
                return_is_explicit: false,
                signature: Some(format!(
                    "(req: import(\"{}\").Req, c: import(\"{}\").Ctx) => void",
                    abs("src/types"),
                    cached("web-kit/4.12.12")
                )),
                intent_input_hash: None,
                dispatch_table: None,
            },
        );

        let mut packages = Packages {
            source_paths: vec![PathBuf::from(abs("package.json"))],
            ..Packages::default()
        };
        packages.merged_dependencies.insert(
            "express".to_string(),
            PackageInfo {
                name: "express".to_string(),
                version: "4.18.0".to_string(),
                spec: "^4.18.0".to_string(),
                source_path: PathBuf::from(abs("package.json")),
            },
        );

        let mut file_results = HashMap::new();
        file_results.insert(
            abs("src/providers/search.ts"),
            crate::agents::file_analyzer_agent::FileAnalysisResult::default(),
        );

        let mut data = CloudRepoData {
            repo_name: "acme-app".to_string(),
            service_name: None,
            endpoints: vec![op("src/routes/orders.ts:18")],
            calls: vec![op("src/providers/search.ts:313")],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions,
            config_json: None,
            package_json: serde_json::to_string(&packages).ok(),
            packages: Some(packages),
            last_updated: chrono::Utc::now(),
            commit_hash: "test".to_string(),
            dirty: None,
            mount_graph: Some(graph),
            bundled_types: Some(format!(
                "export type Order = import(\"{}\").Order;\nexport type Ctx = import(\"{}\").Context;\n",
                abs("src/types/order"),
                cached("web-kit/4.12.12")
            )),
            type_manifest: Some(vec![TypeManifestEntry {
                key: OperationKey::http("GET", "/orders".to_string()),
                role: ManifestRole::Producer,
                type_kind: ManifestTypeKind::Response,
                type_alias: "Endpoint_Response".to_string(),
                file_path: abs("src/routes/orders.ts"),
                line_number: 18,
                is_explicit: true,
                type_state: ManifestTypeState::Explicit,
                evidence: TypeEvidence {
                    file_path: abs("src/routes/orders.ts"),
                    span_start: None,
                    span_end: None,
                    line_number: 18,
                    infer_kind: InferKind::Expression,
                    is_explicit: true,
                    type_state: ManifestTypeState::Explicit,
                },
                resolved_definition: Some(format!(
                    "export type Endpoint_Response = import(\"{}\").Order;",
                    abs("src/types/order")
                )),
                expanded_definition: Some(format!(
                    "{{ order: import(\"{}\").Order; ctx: import(\"{}\").Context; }}",
                    abs("src/types/order"),
                    cached("@acme/schema/9.6.0")
                )),
                primary_type_symbol: None,
                defined_in: None,
                any_provenance: Vec::new(),
                unwidened_definition: None,
                v1_state_before_demotion: None,
            }]),
            file_results: Some(file_results),
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: Some(crate::cloud_storage::CaptureStubArtifact {
                artifact_version: 1,
                package_name: "@carrick/acme-app".to_string(),
                ts_version: "5.8.2".to_string(),
                bare_checkout: false,
                files: std::collections::BTreeMap::from([
                    (
                        CAPTURE_MANIFEST_FILE.to_string(),
                        serde_json::json!({
                            "aliases": [
                                {
                                    "alias": "Endpoint_Response",
                                    "source_file": abs("@/features/types"),
                                    "self_check_detail": format!(
                                        "declaration emit was skipped for module '../../../../../..{}'; alias demoted",
                                        cached("result-kit/8.2.0")
                                    ),
                                    "capture_failure_reason": format!(
                                        "source file not in program: {}",
                                        abs("packages/utils/id.ts")
                                    ),
                                }
                            ]
                        })
                        .to_string(),
                    ),
                    (
                        // What the capture emits since carrick#1174: an in-tree
                        // relative specifier and a bare package specifier.
                        "types/surface.d.ts".to_string(),
                        STUB_SURFACE.to_string(),
                    ),
                    (
                        // A path the capture's rewrite did not recognise slips
                        // through (carrick#1204): only the upload boundary
                        // stands between it and the index.
                        "types/slipped.d.ts".to_string(),
                        format!(
                            "export type Slipped = import(\"{}\").T | import(\"{}\").U;\n",
                            abs("node_modules/.unknown-layout/kit/index"),
                            cached("kit/1.0.0")
                        ),
                    ),
                ]),
            }),
            external_call_candidates: Some(vec![ExternalCallCandidate {
                file: abs("src/providers/search.ts"),
                line: 313,
                callee: "GET".to_string(),
                package: "SEARCH_URL".to_string(),
                mechanism: CallMechanism::EnvVarUrl,
                import_symbol: None,
                subpath: None,
            }]),
            sdk_surface: Some(vec![crate::sdk_surface::SdkMember {
                export: "default".to_string(),
                chain: "payments.create".to_string(),
                file: abs("src/resources/payments.ts"),
                line: 28,
                end_line: 33,
                delegates: vec![crate::sdk_surface::SdkSpan {
                    file: abs("src/api/payments.ts"),
                    line: 12,
                    end_line: 18,
                }],
                subpaths: vec![".".to_string()],
            }]),
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        relativize_cloud_paths(
            &mut data,
            repo_path,
            &served_paths::PathScrub::new(repo_path, Some(home)),
        );

        // Spot-check the field the CI comment actually renders, so a walk that
        // silently stopped finding strings can't pass this test.
        let graph = data.mount_graph.as_ref().expect("graph");
        assert_eq!(
            graph.data_calls[0].file_location,
            "src/providers/search.ts:313"
        );
        assert_eq!(
            graph.data_calls[0].reaches_request.as_deref(),
            Some("src/lib/search-client.ts:88"),
            "the request a wrapper call reaches is a location like any other"
        );
        assert_eq!(graph.endpoints[0].file_location, "src/routes/orders.ts:18");
        assert_eq!(
            data.calls[0].file_path,
            PathBuf::from("src/providers/search.ts:313")
        );
        assert_eq!(
            data.bundled_types.as_deref(),
            Some(
                "export type Order = import(\"src/types/order\").Order;\nexport type Ctx = import(\"web-kit@4.12.12\").Context;\n"
            ),
            "the compiler leaks the absolute root into the bundle as import(\"...\")"
        );
        // Every printed type on a function row is served on its own, not only
        // the signature composed from them.
        let handler = &data.function_definitions["handler"];
        assert_eq!(
            handler.signature.as_deref(),
            Some("(req: import(\"src/types\").Req, c: import(\"web-kit@4.12.12\").Ctx) => void")
        );
        assert_eq!(
            handler.return_type.as_deref(),
            Some("Promise<import(\"web-kit@4.12.12\").TypedResponse<any>>")
        );
        assert_eq!(
            handler.arguments[0].type_string.as_deref(),
            Some("import(\"@acme/kit@2.0.1\").Ctx")
        );
        // Printed TypeScript carries the same leak, and `expanded_definition`
        // is what a mismatch row prints as the type label. The JSON sweep below
        // cannot see these: they start with `export type` / `import(`, not with
        // the root.
        let entry = &data.type_manifest.as_ref().expect("manifest")[0];
        assert_eq!(
            entry.resolved_definition.as_deref(),
            Some("export type Endpoint_Response = import(\"src/types/order\").Order;")
        );
        assert_eq!(
            entry.expanded_definition.as_deref(),
            Some(
                "{ order: import(\"src/types/order\").Order; ctx: import(\"@acme/schema@9.6.0\").Context; }"
            )
        );
        let stub = data.capture_stub.as_ref().expect("capture stub");
        let record: serde_json::Value =
            serde_json::from_str(&stub.files[CAPTURE_MANIFEST_FILE]).expect("record stays JSON");
        assert_eq!(
            record["aliases"][0]["self_check_detail"],
            "declaration emit was skipped for module 'result-kit@8.2.0'; alias demoted"
        );
        assert_eq!(
            record["aliases"][0]["capture_failure_reason"],
            "source file not in program: packages/utils/id.ts"
        );
        assert_eq!(record["aliases"][0]["source_file"], "@/features/types");
        // The pass never edits a compiled declaration file, byte for byte.
        assert_eq!(stub.files["types/surface.d.ts"], STUB_SURFACE);

        let offenders_in = |payload: &CloudRepoData| {
            let json = serde_json::to_value(payload).expect("payload serializes");
            let mut offenders: Vec<String> = Vec::new();
            walk_json_strings(&json, &mut |s| {
                if s.contains(repo_path) || s.contains(home) {
                    offenders.push(s.to_string());
                }
            });
            offenders
        };
        // The served-text pass alone leaves exactly the slipped stub file, so
        // it is the upload boundary below that makes the sweep pass.
        let after_relativize = offenders_in(&data);
        assert_eq!(after_relativize.len(), 1, "{after_relativize:?}");
        assert!(after_relativize[0].starts_with("export type Slipped"));

        // Then the upload boundary, and the exhaustive sweep over what it
        // hands the upload: every string and key, every stub file included.
        let outgoing = upload_boundary::UploadBoundary::new(repo_path, Some(home))
            .scrub(&data, "acme-app")
            .expect("the boundary rewrites the slipped file");
        let offenders = offenders_in(&outgoing);
        assert!(
            offenders.is_empty(),
            "uploaded payload still carries absolute paths: {:?}",
            offenders
        );
        let outgoing_stub = outgoing.capture_stub.as_ref().expect("capture stub");
        assert_eq!(
            outgoing_stub.files["types/slipped.d.ts"],
            "export type Slipped = import(\"<checkout>/node_modules/.unknown-layout/kit/index\").T \
             | import(\"<home>/.cache/runtime/npm/registry.npmjs.org/kit/1.0.0/dist/types\").U;\n"
        );
        assert_eq!(
            outgoing_stub.files["types/surface.d.ts"], STUB_SURFACE,
            "a file with no machine path is uploaded as it is"
        );
        // The boundary rebuilt the payload from its own wire format; nothing
        // else in it moved.
        let mut expected = serde_json::to_value(&data).unwrap();
        expected["capture_stub"]["files"]["types/slipped.d.ts"] =
            serde_json::Value::String(outgoing_stub.files["types/slipped.d.ts"].clone());
        assert_eq!(serde_json::to_value(&outgoing).unwrap(), expected);
    }

    /// Visit every string in a JSON value, keys included: an absolute path can
    /// be a map KEY (the `file_results` cache) as easily as a value.
    fn walk_json_strings(value: &serde_json::Value, visit: &mut impl FnMut(&str)) {
        match value {
            serde_json::Value::String(s) => visit(s),
            serde_json::Value::Array(items) => {
                for item in items {
                    walk_json_strings(item, visit);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    visit(key);
                    walk_json_strings(item, visit);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn test_ast_stripping_removes_nodes() {
        // Create test CloudRepoData with AST nodes
        let endpoint = ApiEndpointDetails {
            view_module: false,
            owner: Some(OwnerType::App("test_app".to_string())),
            key: OperationKey::http("GET", "/test"),
            params: vec![],
            request_body: None,
            response_body: None,
            file_path: PathBuf::from("test.js"),
            request_type: Some(TypeReference {
                file_path: PathBuf::from("test.ts"),
                type_ann: None,
                start_position: 0,
                composite_type_string: "TestType".to_string(),
                alias: "TestType".to_string(),
            }),
            response_type: Some(TypeReference {
                file_path: PathBuf::from("test.ts"),
                type_ann: None,
                start_position: 0,
                composite_type_string: "ResponseType".to_string(),
                alias: "ResponseType".to_string(),
            }),
            handler_name: Some("testHandler".to_string()),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: None,
            schema_binding: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
        };

        let test_data = CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![endpoint.clone()],
            calls: vec![endpoint.clone()],
            mounts: vec![],
            apps: std::collections::HashMap::new(),
            imported_handlers: vec![],
            function_definitions: std::collections::HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        // Verify strip_ast_nodes removes AST nodes
        let stripped = strip_ast_nodes(test_data, true);

        assert!(stripped.endpoints[0].request_type.is_none());
        assert!(stripped.endpoints[0].response_type.is_none());
        assert!(stripped.calls[0].request_type.is_none());
        assert!(stripped.calls[0].response_type.is_none());
    }

    #[test]
    fn test_merge_serialized_data() {
        use crate::config::Config;
        use crate::packages::Packages;

        let test_data = vec![CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: std::collections::HashMap::new(),
            imported_handlers: vec![],
            function_definitions: std::collections::HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        }];

        // Test Config merging
        let merged_config: Result<Config, _> =
            merge_serialized_data(&test_data, |data| data.config_json.as_ref());
        assert!(merged_config.is_ok());

        // Test Packages merging
        let merged_packages: Result<Packages, _> =
            merge_serialized_data(&test_data, |data| data.package_json.as_ref());
        assert!(merged_packages.is_ok());

        // Test with empty data returns default
        let empty_data: Vec<CloudRepoData> = vec![];
        let default_config: Result<Config, _> =
            merge_serialized_data(&empty_data, |data| data.config_json.as_ref());
        assert!(default_config.is_ok());
    }

    #[tokio::test]
    async fn test_cross_repo_analyzer_builder_no_sourcemap_issues() {
        use crate::analyzer::builder::AnalyzerBuilder;
        use crate::config::Config;
        use swc_common::{SourceMap, sync::Lrc};

        // Create test data with TypeReferences that would cause SourceMap issues
        let endpoint = ApiEndpointDetails {
            view_module: false,
            owner: Some(OwnerType::App("test_app".to_string())),
            key: OperationKey::http("GET", "/test"),
            params: vec![],
            request_body: None,
            response_body: None,
            file_path: PathBuf::from("test.js"),
            request_type: Some(TypeReference {
                file_path: PathBuf::from("test.ts"),
                type_ann: None,
                start_position: 999999, // This would cause SourceMap issues
                composite_type_string: "TestType".to_string(),
                alias: "TestType".to_string(),
            }),
            response_type: Some(TypeReference {
                file_path: PathBuf::from("test.ts"),
                type_ann: None,
                start_position: 999999, // This would cause SourceMap issues
                composite_type_string: "ResponseType".to_string(),
                alias: "ResponseType".to_string(),
            }),
            handler_name: Some("testHandler".to_string()),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: None,
            schema_binding: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
        };

        let test_data = vec![CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![endpoint.clone()],
            calls: vec![endpoint.clone()],
            mounts: vec![],
            apps: std::collections::HashMap::new(),
            imported_handlers: vec![],
            function_definitions: std::collections::HashMap::new(),
            config_json: Some(r#"{"ignore_patterns": [], "type_check": false}"#.to_string()),
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        }];

        // Test that cross-repo builder doesn't fail with SourceMap issues
        let cm: Lrc<SourceMap> = Default::default();
        let config = Config::default();
        let builder = AnalyzerBuilder::new_for_cross_repo(config, cm);

        // This should not panic with SourceMap issues
        let result = builder.build_from_repo_data(test_data).await;
        assert!(result.is_ok(), "build_cross_repo_analyzer should not fail");

        let analyzer = result.unwrap();
        assert_eq!(analyzer.endpoints.len(), 1);
        assert_eq!(analyzer.calls.len(), 1);
    }

    // === Incremental analysis tests ===

    use crate::agents::file_analyzer_agent::{DataCallResult, EndpointResult, FileAnalysisResult};

    fn make_file_result(endpoints: Vec<&str>, data_calls: Vec<&str>) -> FileAnalysisResult {
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: endpoints
                .into_iter()
                .map(|path| EndpointResult {
                    handler_declaration_line: None,
                    registration_literal: None,
                    view_module: false,
                    candidate_id: "cand_123".to_string(),
                    line_number: 10,
                    owner_node: "app".to_string(),
                    method: "GET".to_string(),
                    path: path.to_string(),
                    handler_name: "handler".to_string(),
                    pattern_matched: "app.get(...)".to_string(),
                    call_expression_span_start: Some(100),
                    call_expression_span_end: Some(200),
                    payload_expression_text: Some("req.body".to_string()),
                    payload_expression_line: Some(11),
                    response_expression_text: Some("res.json(data)".to_string()),
                    response_expression_line: Some(12),
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                    resolution_source: None,
                    dispatch: None,
                })
                .collect(),
            data_calls: data_calls
                .into_iter()
                .map(|target| DataCallResult {
                    call_kind: None,
                    candidate_id: "cand_456".to_string(),
                    line_number: 20,
                    target: target.to_string(),
                    method: Some("GET".to_string()),
                    pattern_matched: "fetch(...)".to_string(),
                    call_expression_span_start: Some(300),
                    call_expression_span_end: Some(400),
                    call_expression_text: Some("fetch('/api')".to_string()),
                    call_expression_line: Some(21),
                    payload_expression_text: Some("body".to_string()),
                    payload_expression_line: Some(22),
                    primary_type_symbol: None,
                    type_import_source: None,

                    loopback_default_url: None,
                    base: None,
                    consumers_not_resolved: None,
                    resolution_source: None,
                    dispatch: None,
                    reaches_request: None,
                    body_literals: Default::default(),
                    library_semantics: Vec::new(),
                    at_caller: false,
                    call_body: None,
                })
                .collect(),
            graphql_operations: vec![],
            pubsub_operations: vec![],
            dispatch_tables: Vec::new(),
        }
    }

    /// Regression for #102: consumer manifest paths must run through the same
    /// UrlNormalizer::normalize as the live mount-graph matcher. The exact
    /// targets from the bad run (backticked template literals with env-var
    /// base URLs) previously surfaced in the consumer manifest as
    /// `/:USER_SERVICE_URL/api/users/:order.userId`, so ts_check reported
    /// orphans the live matcher had already correlated.
    #[test]
    fn test_consumer_manifest_paths_strip_env_var_base_urls() {
        // Declared-internal env-var bases must be stripped from the consumer
        // manifest key. The canonical path is computed ONCE (via
        // `consumer_call_path`) at mount-graph build time and stored on the call;
        // `build_type_manifest_entries` reads that stored `canonical_path` so the
        // manifest key is byte-identical to the projection key for the same call.
        let config = Config {
            internal_env_vars: ["USER_SERVICE_URL", "NOTIFICATION_SERVICE_URL"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Config::default()
        };
        let normalizer = UrlNormalizer::new(&config);
        let mk_call = |target: &str, file: &str| {
            let target = target.to_string();
            crate::mount_graph::DataFetchingCall {
                method: "GET".to_string(),
                canonical_path: normalizer.consumer_call_path(&target),
                target_url: target,
                client: "fetch".to_string(),
                file_location: file.to_string(),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: None,
                line: None,
                base: None,
                consumers_not_resolved: None,
                resolution_source: None,
                dispatch: None,
                role: None,
                reaches_request: None,
                library_semantics: Vec::new(),
            }
        };
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![
            mk_call(
                "`${USER_SERVICE_URL}/api/users/${order.userId}`",
                "src/orders.ts:42",
            ),
            mk_call(
                "`${NOTIFICATION_SERVICE_URL}/api/notifications/status`",
                "src/notify.ts:7",
            ),
        ];

        let entries = build_type_manifest_entries(&mount_graph, &config, ".");

        let consumer_paths: Vec<&str> = entries
            .iter()
            .filter(|e| e.role == ManifestRole::Consumer)
            .filter_map(|e| e.key.as_http().map(|(_, path)| path))
            .collect();
        assert!(!consumer_paths.is_empty(), "consumer entries expected");
        for path in &consumer_paths {
            assert!(
                !path.contains("_SERVICE_URL"),
                "env-var base URL leaked into consumer manifest path: {}",
                path
            );
        }
        // F3c: the member expression `${order.userId}` now collapses to the clean
        // segment `:userId` rather than the malformed `:order.userId`. Param names
        // are matching-agnostic, so this is purely a well-formedness improvement.
        assert!(
            consumer_paths.contains(&"/api/users/:userId"),
            "expected normalized user-service path, got {:?}",
            consumer_paths
        );
        assert!(
            consumer_paths.contains(&"/api/notifications/status"),
            "expected normalized notification path, got {:?}",
            consumer_paths
        );
    }

    /// carrick#1601: a row restated at a helper's caller states no response
    /// contract, so its consumer response entry goes and the site is no party
    /// to a response verdict. Its request entry stays, and so does every entry
    /// of a request line's own row, including another verb on the caller's
    /// line.
    ///
    /// carrick#1782: a row at a call to a function that builds its own body
    /// (or sends none) states no REQUEST contract, whichever pass stated it,
    /// so its request entry goes too; a row whose function sends a declared
    /// parameter unchanged keeps its request entry.
    #[test]
    fn a_row_restated_at_a_helper_s_caller_has_no_response_entry() {
        let config = Config::default();
        let call = |method: &str, path: &str, line: u32| crate::mount_graph::DataFetchingCall {
            method: method.to_string(),
            canonical_path: path.to_string(),
            target_url: path.to_string(),
            client: "fetch".to_string(),
            file_location: format!("src/page.ts:{line}"),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
            role: None,
            reaches_request: None,
            library_semantics: Vec::new(),
        };
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![
            call("GET", "/things/:id/availability", 4),
            call("POST", "/pdf", 9),
            call("GET", "/orders", 9),
            call("GET", "/orders", 12),
            call("POST", "/orders/:id/notes", 15),
            call("GET", "/orders/:id", 15),
            call("POST", "/orders/:id/tags", 18),
            call("PUT", "/orders/:id/status", 21),
        ];
        let row = |method: &str, line: i32, at_caller: bool| DataCallResult {
            call_kind: None,
            candidate_id: format!("span:{line}"),
            line_number: line,
            target: "/x".to_string(),
            method: Some(method.to_string()),
            pattern_matched: "helper".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: None,
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: Some(
                crate::agents::file_analyzer_agent::ResolutionSource::RequestSummary,
            ),
            dispatch: None,
            reaches_request: None,
            body_literals: Default::default(),
            library_semantics: Vec::new(),
            at_caller,
            call_body: None,
        };
        let declared =
            crate::forwarded_body::CallBody::Param(crate::forwarded_body::DeclaredParam {
                file: "src/orders.api.ts".to_string(),
                span_start: 120,
                span_end: 126,
                line: 7,
            });
        let mut file_results = HashMap::new();
        file_results.insert(
            "src/page.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    row("GET", 4, true),
                    row("POST", 9, true),
                    row("GET", 9, false),
                    row("GET", 12, false),
                    DataCallResult {
                        resolution_source: Some(
                            crate::agents::file_analyzer_agent::ResolutionSource::ImportedMember,
                        ),
                        call_body: Some(crate::forwarded_body::CallBody::Built),
                        ..row("POST", 15, false)
                    },
                    row("GET", 15, false),
                    DataCallResult {
                        call_body: Some(crate::forwarded_body::CallBody::Built),
                        ..row("POST", 18, true)
                    },
                    DataCallResult {
                        call_body: Some(declared),
                        ..row("PUT", 21, true)
                    },
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        let mut entries = build_type_manifest_entries(&mount_graph, &config, ".");
        drop_call_through_entries(&mut entries, &file_results);

        let mut kept: Vec<(u32, String, ManifestTypeKind)> = entries
            .iter()
            .filter(|entry| entry.role == ManifestRole::Consumer)
            .map(|entry| {
                (
                    entry.line_number,
                    entry
                        .key
                        .as_http()
                        .map(|(method, _)| method.to_string())
                        .unwrap_or_default(),
                    entry.type_kind,
                )
            })
            .collect();
        kept.sort_by_key(|(line, method, kind)| {
            (*line, method.clone(), *kind == ManifestTypeKind::Response)
        });
        assert_eq!(
            kept,
            vec![
                (4, "GET".to_string(), ManifestTypeKind::Request),
                (9, "GET".to_string(), ManifestTypeKind::Request),
                (9, "GET".to_string(), ManifestTypeKind::Response),
                (9, "POST".to_string(), ManifestTypeKind::Request),
                (12, "GET".to_string(), ManifestTypeKind::Request),
                (12, "GET".to_string(), ManifestTypeKind::Response),
                (15, "GET".to_string(), ManifestTypeKind::Request),
                (15, "GET".to_string(), ManifestTypeKind::Response),
                (15, "POST".to_string(), ManifestTypeKind::Response),
                (21, "PUT".to_string(), ManifestTypeKind::Request),
            ]
        );
    }

    /// #334, as carrick#718 resolved it: two producer endpoints on one
    /// (method, path) each get their own manifest entry, because a producer's
    /// alias now carries its declaration site.
    ///
    /// #334 was the collision itself — both entries took the key-only alias and
    /// one resolved definition clobbered the other in the bundle — and until
    /// #718 it was answered by DROPPING the second endpoint here. That was a
    /// reasonable answer while the live trigger was a mis-extracted duplicate
    /// route (#332, a root route emitted as "/:id"), and the wrong one once
    /// carrick#704 made two real producers at one path the normal shape of a
    /// file-routed app: a pathless layout and the page beneath it both serve
    /// the parent path, and reporting one's response type for both is a
    /// statement about a contract the other does not offer. Which one survived
    /// depended on the order this loop saw them in, so it read as a
    /// determinism failure too.
    #[test]
    fn test_duplicate_producer_keys_each_get_their_own_manifest_entry() {
        let config = Config::default();
        let mk_endpoint = |file: &str| crate::mount_graph::ResolvedEndpoint {
            view_module: false,
            method: "GET".to_string(),
            path: "/api/orders/:id".to_string(),
            full_path: "/api/orders/:id".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: file.to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            dispatch: None,
            handler_span: None,
        };
        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints = vec![
            mk_endpoint("src/routes/orders.ts:11"),
            mk_endpoint("src/routes/orders.ts:42"),
        ];

        let entries = build_type_manifest_entries(&mount_graph, &config, ".");

        let producer_aliases: Vec<&str> = entries
            .iter()
            .filter(|e| e.role == ManifestRole::Producer)
            .map(|e| e.type_alias.as_str())
            .collect();
        let unique: std::collections::HashSet<&&str> = producer_aliases.iter().collect();
        assert_eq!(
            producer_aliases.len(),
            unique.len(),
            "same-key producers must not share a manifest alias: {:?}",
            producer_aliases
        );
        // Neither is dropped: both declarations are facts about the service.
        assert_eq!(
            producer_aliases.len(),
            2,
            "both same-key producers must keep an entry: {:?}",
            producer_aliases
        );
        let mut sites: Vec<u32> = entries
            .iter()
            .filter(|e| e.role == ManifestRole::Producer)
            .map(|e| e.line_number)
            .collect();
        sites.sort_unstable();
        assert_eq!(
            sites,
            vec![11, 42],
            "each entry keeps the line it was declared at"
        );
        // The site is what separates them, so it is in the alias itself: the
        // two agree on everything up to the `_At<id>` suffix.
        let common_prefix = producer_aliases[0]
            .char_indices()
            .zip(producer_aliases[1].chars())
            .take_while(|((_, a), b)| a == b)
            .count();
        assert!(
            producer_aliases[0][..common_prefix].contains("_Response")
                || producer_aliases[0][..common_prefix].contains("_Request"),
            "the aliases must differ only in the site suffix: {:?}",
            producer_aliases
        );
    }

    /// #379: a call-site-evidence entry never anchors Producer manifest
    /// types — a producer entry would make the type check run a
    /// request-vs-request comparison mislabelled as a producer-contract
    /// verdict. The twin data call's Consumer entries are unaffected.
    #[test]
    fn test_call_site_evidence_endpoint_emits_no_producer_manifest_entries() {
        let config = Config::default();
        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints = vec![crate::mount_graph::ResolvedEndpoint {
            view_module: false,
            method: "POST".to_string(),
            path: "/v2/widgets".to_string(),
            full_path: "/v2/widgets".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "operations/create-widget.ts:14".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::CallSite,
            resolution_source: None,
            dispatch: None,
            handler_span: None,
        }];
        mount_graph.data_calls = vec![crate::mount_graph::DataFetchingCall {
            method: "POST".to_string(),
            target_url: "/v2/widgets".to_string(),
            canonical_path: "/v2/widgets".to_string(),
            client: "request".to_string(),
            file_location: "operations/create-widget.ts:14".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
            role: None,
            reaches_request: None,
            library_semantics: Vec::new(),
        }];

        let entries = build_type_manifest_entries(&mount_graph, &config, ".");

        assert!(
            entries.iter().all(|e| e.role != ManifestRole::Producer),
            "call-site evidence must not produce Producer manifest entries: {:?}",
            entries
                .iter()
                .map(|e| (&e.role, &e.type_alias))
                .collect::<Vec<_>>()
        );
        assert!(
            entries.iter().any(|e| e.role == ManifestRole::Consumer),
            "the twin call's Consumer entries are still emitted"
        );
    }

    #[test]
    fn test_hash_file_content_deterministic() {
        let hash1 = hash_file_content("hello world");
        let hash2 = hash_file_content("hello world");
        assert_eq!(hash1, hash2);

        let hash3 = hash_file_content("different content");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_normalize_file_results_keys_absolute_path() {
        let mut results = HashMap::new();
        results.insert(
            "/home/user/repo/src/app.ts".to_string(),
            make_file_result(vec!["/api/users"], vec![]),
        );
        results.insert(
            "/home/user/repo/src/routes.ts".to_string(),
            make_file_result(vec!["/api/posts"], vec![]),
        );

        let normalized = normalize_file_results_keys(&results, "/home/user/repo");

        assert!(normalized.contains_key("src/app.ts"));
        assert!(normalized.contains_key("src/routes.ts"));
        assert_eq!(normalized.len(), 2);
    }

    #[test]
    fn test_normalize_file_results_keys_dot_prefix() {
        let mut results = HashMap::new();
        results.insert(
            "./src/app.ts".to_string(),
            make_file_result(vec!["/api/users"], vec![]),
        );

        let normalized = normalize_file_results_keys(&results, ".");

        assert!(normalized.contains_key("src/app.ts"));
    }

    #[test]
    fn test_normalize_file_results_keys_already_relative() {
        let mut results = HashMap::new();
        results.insert(
            "src/app.ts".to_string(),
            make_file_result(vec!["/api/users"], vec![]),
        );

        let normalized = normalize_file_results_keys(&results, "/some/other/path");

        // Key doesn't match prefix, should be kept as-is
        assert!(normalized.contains_key("src/app.ts"));
    }

    /// The reader half of carrick#1079: a file is reusable only while its
    /// bytes on disk are the ones the previous scan's commit holds. Uncommitted
    /// edits, staged or not, and files git does not track are all changed; a
    /// file untouched beside them is not, so one stray file no longer costs
    /// every other file its answer.
    #[test]
    fn reusable_paths_reads_the_working_tree_not_head() {
        let (repo, base) = committed_repo(&[
            ("src/app.ts", "const x = 1;"),
            ("src/staged.ts", "const s = 1;"),
            ("src/kept.ts", "const k = 1;"),
            ("src/caf\u{e9}.ts", "const c = 1;"),
        ]);
        let root = repo.path();
        std::fs::write(root.join("src/app.ts"), "const x = 2;").unwrap();
        std::fs::write(root.join("src/staged.ts"), "const s = 2;").unwrap();
        let add = std::process::Command::new("git")
            .args(["add", "src/staged.ts"])
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(add.status.success());
        std::fs::write(root.join("src/untracked.ts"), "const u = 1;").unwrap();

        let unchanged = reusable_paths(root.to_str().unwrap(), &base).expect("git answers");

        assert!(unchanged.contains("src/kept.ts"), "{unchanged:?}");
        // A non-ASCII path comes back byte-exact, not quoted.
        assert!(unchanged.contains("src/caf\u{e9}.ts"), "{unchanged:?}");
        for changed in ["src/app.ts", "src/staged.ts", "src/untracked.ts"] {
            assert!(
                !unchanged.contains(changed),
                "{changed} differs from the commit on disk: {unchanged:?}"
            );
        }

        // An edit reverted to the commit's bytes is unchanged again.
        std::fs::write(root.join("src/app.ts"), "const x = 1;").unwrap();
        let unchanged = reusable_paths(root.to_str().unwrap(), &base).unwrap();
        assert!(unchanged.contains("src/app.ts"), "{unchanged:?}");
    }

    /// A scan rooted below the repository's top level reads paths relative to
    /// its own root, which is how the cache keys them.
    #[test]
    fn reusable_paths_is_relative_to_a_root_inside_the_repository() {
        let (repo, base) = committed_repo(&[
            ("services/api/src/app.ts", "const x = 1;"),
            ("services/api/src/kept.ts", "const k = 1;"),
            ("other/elsewhere.ts", "const e = 1;"),
        ]);
        std::fs::write(repo.path().join("services/api/src/app.ts"), "const x = 2;").unwrap();
        let root = repo.path().join("services/api");

        let unchanged = reusable_paths(root.to_str().unwrap(), &base).unwrap();

        assert_eq!(
            unchanged,
            HashSet::from(["src/kept.ts".to_string()]),
            "only the service's unchanged file, keyed from the service root"
        );
    }

    #[test]
    fn reusable_paths_is_none_when_git_cannot_answer() {
        let (repo, _) = committed_repo(&[("app.ts", "x")]);
        let root = repo.path().to_str().unwrap();
        // A commit this clone does not hold (what a shallow clone looks like).
        assert!(reusable_paths(root, "0000000000000000000000000000000000000000").is_none());
        // Not a commit id at all never reaches git.
        assert!(reusable_paths(root, "--output=/tmp/x").is_none());
    }

    #[test]
    fn test_payload_size_guard_drops_file_results_when_too_large() {
        // Create CloudRepoData with large file_results
        let mut large_results = HashMap::new();
        // Create entries large enough to exceed 5MB
        for i in 0..1000 {
            let large_string = "x".repeat(5000);
            large_results.insert(
                format!("src/file_{}.ts", i),
                FileAnalysisResult {
                    graphql_consumer_locates: vec![],
                    mounts: vec![],
                    endpoints: vec![EndpointResult {
                        handler_declaration_line: None,
                        registration_literal: None,
                        view_module: false,
                        candidate_id: large_string.clone(),
                        line_number: 1,
                        owner_node: "app".to_string(),
                        method: "GET".to_string(),
                        path: large_string.clone(),
                        handler_name: large_string.clone(),
                        pattern_matched: large_string.clone(),
                        call_expression_span_start: None,
                        call_expression_span_end: None,
                        payload_expression_text: Some(large_string.clone()),
                        payload_expression_line: None,
                        response_expression_text: Some(large_string),
                        response_expression_line: None,
                        emission_style: None,
                        primary_type_symbol: None,
                        type_import_source: None,
                        resolution_source: None,
                        dispatch: None,
                    }],
                    data_calls: vec![],
                    graphql_operations: vec![],
                    pubsub_operations: vec![],
                    dispatch_tables: Vec::new(),
                },
            );
        }

        let data = CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: Some(large_results),
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: Some(CACHE_VERSION),
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        // Staging unavailable: the request body has to carry the payload, so
        // the caches are what gets dropped to fit it under the wall.
        let stripped = strip_ast_nodes(data, false);

        assert!(
            stripped.file_results.is_none(),
            "file_results should be dropped when the payload exceeds 5MB and \
             the backend cannot stage it"
        );
    }

    #[test]
    fn test_payload_size_guard_keeps_small_file_results() {
        let mut small_results = HashMap::new();
        small_results.insert(
            "src/app.ts".to_string(),
            make_file_result(vec!["/api/users"], vec![]),
        );

        let data = CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: Some(small_results),
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: Some(CACHE_VERSION),
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        let stripped = strip_ast_nodes(data, true);

        // file_results should be preserved (small payload)
        assert!(
            stripped.file_results.is_some(),
            "file_results should be preserved when payload is small"
        );
    }

    /// carrick#536 regression: the guard used to drop the incremental caches
    /// from every payload over 5MB, without asking whether that payload was
    /// going to travel in the request body at all. It was not — anything over
    /// the 5MB guard is also over [`INLINE_PAYLOAD_LIMIT_BYTES`], so
    /// `upload_repo_data` stages it to S3 and the request carries a pointer.
    /// Large repos lost their caches and re-analyzed every file on the next
    /// scan for nothing. Same oversized payload, both answers from the backend.
    #[test]
    fn test_payload_size_guard_keeps_caches_when_the_payload_will_be_staged() {
        const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024; // mirrors the guard

        fn oversized() -> CloudRepoData {
            let mut result = make_file_result(vec!["/api/orders"], vec![]);
            result.endpoints[0].payload_expression_text = Some("x".repeat(MAX_PAYLOAD_BYTES));
            let mut file_results = HashMap::new();
            file_results.insert("src/big.ts".to_string(), result);
            CloudRepoData {
                repo_name: "orders-svc".to_string(),
                service_name: None,
                endpoints: vec![],
                calls: vec![],
                mounts: vec![],
                apps: HashMap::new(),
                imported_handlers: vec![],
                function_definitions: HashMap::new(),
                config_json: None,
                package_json: None,
                packages: None,
                last_updated: chrono::Utc::now(),
                commit_hash: "test-hash".to_string(),
                dirty: None,
                mount_graph: None,
                bundled_types: None,
                type_manifest: None,
                file_results: Some(file_results),
                cached_detection: Some(crate::framework_detector::DetectionResult {
                    frameworks: vec!["express".to_string()],
                    data_fetchers: vec![],
                    messaging_clients: vec![],
                    socket_clients: vec![],
                    notes: String::new(),
                    client_semantics: None,
                }),
                cached_guidance: None,
                cached_extraction_config: None,
                package_json_hash: None,
                cache_version: Some(CACHE_VERSION),
                type_extraction_status: None,
                types_degraded: None,
                compat_verdicts: None,
                capture_stub: None,
                external_call_candidates: None,
                sdk_surface: None,
                sdk_edges: None,
                sdk_unresolved: None,
                scanner_version: None,
                scanner_build: None,
                boundary: None,
                dispatch_tables: None,
            }
        }

        // Test setup: the payload is over the guard's threshold, and therefore
        // also over the inline limit the staging decision measures against.
        let len = serde_json::to_string(&oversized()).unwrap().len();
        assert!(len > MAX_PAYLOAD_BYTES);
        assert!(len > INLINE_PAYLOAD_LIMIT_BYTES);

        let mut staged = oversized();
        enforce_payload_size_limit(&mut staged, true);
        assert!(
            staged.file_results.is_some(),
            "a payload the upload path will stage keeps its file_results cache"
        );
        assert!(
            staged.cached_detection.is_some(),
            "a payload the upload path will stage keeps its detection cache"
        );

        let mut inlined = oversized();
        enforce_payload_size_limit(&mut inlined, false);
        assert!(
            inlined.file_results.is_none(),
            "without staging the request body must fit, so the caches are dropped"
        );
        assert!(
            inlined.cached_detection.is_none(),
            "without staging the detection cache is dropped too"
        );
    }

    /// #351 regression: `attach_compat_verdicts` runs AFTER the strip_ast_nodes
    /// size-guard pass, so verdicts appended to a payload that squeaked under
    /// the 5MB cap could re-inflate it past what the request body can carry.
    /// The engine re-applies `enforce_payload_size_limit` after
    /// attachment; this pins that a near-limit payload with verdicts attached
    /// still respects the cap, with the SAME degradation order as the first
    /// pass — the bulky caches are dropped, the tiny verdicts are kept.
    #[test]
    fn test_payload_size_guard_reapplied_after_verdict_attachment() {
        const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024; // mirrors the guard

        let base = CloudRepoData {
            repo_name: "consumer-svc".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: Some(CACHE_VERSION),
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        // Size the file_results filler so the payload lands just UNDER the 5MB
        // cap: the strip_ast_nodes guard pass keeps everything, and only the
        // verdicts appended afterwards push it over.
        let base_len = serde_json::to_string(&base).unwrap().len();
        let mut result = make_file_result(vec!["/api/orders"], vec![]);
        result.endpoints[0].payload_expression_text = Some(String::new());
        let mut probe = HashMap::new();
        probe.insert("src/big.ts".to_string(), result.clone());
        let mut with_empty = base.clone();
        with_empty.file_results = Some(probe);
        let overhead = serde_json::to_string(&with_empty).unwrap().len() - base_len;
        // 200 bytes of headroom below the cap — less than the verdicts add.
        let filler_len = MAX_PAYLOAD_BYTES - base_len - overhead - 200;
        result.endpoints[0].payload_expression_text = Some("x".repeat(filler_len));
        let mut file_results = HashMap::new();
        file_results.insert("src/big.ts".to_string(), result);
        let mut data = base;
        data.file_results = Some(file_results);

        // First guard pass (as strip_ast_nodes runs it): under the cap, so the
        // caches survive.
        let mut payloads = vec![strip_ast_nodes(data, false)];
        assert!(
            payloads[0].file_results.is_some(),
            "test setup: payload must start under the cap with caches intact"
        );
        let before = serde_json::to_string(&payloads[0]).unwrap().len();
        assert!(before <= MAX_PAYLOAD_BYTES);

        // Verdict attachment re-inflates past the cap (mismatch reason > the
        // 200-byte headroom).
        let matches = vec![crate::analyzer::CrossRepoMatch {
            producer_repo: "producer-svc".to_string(),
            producer_key: "http|GET|/api/orders/:id".to_string(),
            consumer_repo: "consumer-svc".to_string(),
            consumer_key: "http|GET|/api/orders/:id".to_string(),
            consumer_location: Some("src/client.ts".to_string()),
            match_score: 1.0,
            type_compatible: Some(false),
            type_verdict: Some(crate::operation::TypeVerdict::Incompatible),
            mismatch_reason: Some("y".repeat(400)),
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }];
        // Persistence keys on the check's own outcomes (carrick#811/#822), so a
        // match with no outcome filed for it stores nothing and this setup
        // would not inflate anything.
        let outcomes = vec![crate::analyzer::PairCheckOutcome {
            pair_key: "p/Order~c/src/client.ts".to_string(),
            pseudo_method: "GET".to_string(),
            identity: "/api/orders/:id".to_string(),
            consumer_file: "src/client.ts".to_string(),
            consumer_line: 1,
            type_kind: crate::cloud_storage::ManifestTypeKind::Response,
            bucket: crate::services::type_sidecar::VerdictBucket::Incompatible,
            gate: None,
            diagnostic: Some("y".repeat(400)),
            producer_alias: "Order".to_string(),
            consumer_alias: "OrderView".to_string(),
            producer_service: "producer-svc".to_string(),
            consumer_service: "consumer-svc".to_string(),
            resolved: true,
            unresolved_reason: None,
            notes: Vec::new(),
            consumer_reads: Vec::new(),
        }];
        crate::cloud_storage::attach_compat_verdicts(
            &mut payloads,
            &matches,
            &crate::analyzer::PairDirections::from_outcomes(&outcomes),
        );
        assert!(
            serde_json::to_string(&payloads[0]).unwrap().len() > MAX_PAYLOAD_BYTES,
            "test setup: verdicts must push the payload over the cap"
        );

        // The engine's post-attachment pass: back under the cap, caches
        // dropped, verdicts kept.
        enforce_payload_size_limit(&mut payloads[0], false);
        let after = serde_json::to_string(&payloads[0]).unwrap().len();
        assert!(
            after <= MAX_PAYLOAD_BYTES,
            "payload must respect the cap after verdict attachment ({}KB > {}KB)",
            after / 1024,
            MAX_PAYLOAD_BYTES / 1024
        );
        assert!(
            payloads[0].file_results.is_none(),
            "the bulky caches are what gets dropped"
        );
        assert!(
            payloads[0]
                .compat_verdicts
                .as_ref()
                .is_some_and(|v| v.len() == 1),
            "the tiny verdicts are kept"
        );
    }

    /// The selection the incremental branch makes: previous run had A, B, C;
    /// this run discovers A, B, D; B's content moved. Only A still holds a
    /// usable model answer. C is gone, D was never asked about, and B changed —
    /// each of the other three goes to the model.
    ///
    /// Nothing here decides what is ANALYSED: every discovered file is, on
    /// every scan. This decides only what a scan pays the model for.
    #[test]
    fn only_unchanged_files_the_previous_scan_answered_replay_their_answer() {
        let mut previous = HashMap::new();
        previous.insert(
            "src/a.ts".to_string(),
            make_file_result(vec!["/api/a"], vec![]),
        );
        previous.insert(
            "src/b.ts".to_string(),
            make_file_result(vec!["/api/b"], vec![]),
        );
        previous.insert(
            "src/c.ts".to_string(),
            make_file_result(vec!["/api/c"], vec![]),
        );

        let discovered = ["src/a.ts", "src/b.ts", "src/d.ts"]
            .into_iter()
            .map(|relative| (format!("/repo/{relative}"), relative.to_string()));
        // Git vouches for A and C; B changed; D is not tracked.
        let unchanged: HashSet<String> = ["src/a.ts".to_string(), "src/c.ts".to_string()]
            .into_iter()
            .collect();

        let reusable = reusable_model_answers(discovered, &previous, &unchanged);

        assert_eq!(
            reusable.keys().collect::<Vec<_>>(),
            vec!["/repo/src/a.ts"],
            "only the unchanged, previously-answered file replays"
        );
        // Keyed the way the orchestrator keys a file, carrying the previous
        // scan's answer verbatim.
        assert_eq!(reusable["/repo/src/a.ts"].endpoints[0].path, "/api/a");
    }

    #[test]
    fn test_file_results_serialization_roundtrip() {
        // Verify FileAnalysisResult survives JSON serialization (critical for AWS cache)
        let result = make_file_result(vec!["/api/users", "/api/posts"], vec!["/external/api"]);

        let json = serde_json::to_string(&result).expect("should serialize");
        let deserialized: FileAnalysisResult =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(deserialized.endpoints.len(), 2);
        assert_eq!(deserialized.data_calls.len(), 1);
        assert_eq!(deserialized.endpoints[0].path, "/api/users");
        assert_eq!(deserialized.data_calls[0].target, "/external/api");
        // The candidate span must survive the cache: the incremental path
        // rebuilds the mount graph from these rehydrated results, and
        // `build_mount_graph` reads `call_expression_span_start` to tell a real
        // client call site from a wrapper-resolution echo. If it were dropped
        // here, every rehydrated call would look candidate-less and the
        // suppression would silently stop firing on incremental scans.
        assert_eq!(
            deserialized.data_calls[0].call_expression_span_start,
            Some(300)
        );
    }

    #[test]
    fn test_cloud_repo_data_with_file_results_roundtrip() {
        // Verify CloudRepoData with file_results survives JSON roundtrip
        let mut file_results = HashMap::new();
        file_results.insert(
            "src/app.ts".to_string(),
            make_file_result(vec!["/api/users"], vec![]),
        );

        let data = CloudRepoData {
            repo_name: "express-single".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: Some(file_results),
            cached_detection: Some(DetectionResult {
                frameworks: vec!["express".to_string()],
                data_fetchers: vec!["fetch".to_string()],
                messaging_clients: vec![],
                socket_clients: vec![],
                notes: "test".to_string(),
                client_semantics: None,
            }),
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: Some("abc123hash".to_string()),
            cache_version: Some(CACHE_VERSION),
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        let json = serde_json::to_string(&data).expect("should serialize");
        // A detection never asked for library semantics writes no key for
        // them (carrick#1564), so the blob is the shape it always was.
        assert!(!json.contains("client_semantics"));
        let deserialized: CloudRepoData = serde_json::from_str(&json).expect("should deserialize");

        assert!(deserialized.file_results.is_some());
        let fr = deserialized.file_results.unwrap();
        assert!(fr.contains_key("src/app.ts"));
        assert_eq!(fr["src/app.ts"].endpoints[0].path, "/api/users");

        assert!(deserialized.cached_detection.is_some());
        let cached = deserialized.cached_detection.unwrap();
        assert_eq!(cached.frameworks, vec!["express"]);
        assert_eq!(cached.client_semantics, None, "read back as never asked");
        assert_eq!(deserialized.cache_version, Some(CACHE_VERSION));
        assert_eq!(
            deserialized.package_json_hash,
            Some("abc123hash".to_string())
        );
    }

    /// carrick#1564: the blob's `cached_detection` carries the library
    /// semantics as detection answered them, pending and skipped entries
    /// included, so the next scan can tell what is still unanswered.
    #[test]
    fn cached_detection_round_trips_its_library_semantics() {
        let sample: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .unwrap();
        let persisted = serde_json::to_value(&sample).unwrap();
        let back: DetectionResult = serde_json::from_value(persisted.clone()).unwrap();
        assert_eq!(back.client_semantics, sample.client_semantics);
        let entries = back.client_semantics.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.status).collect::<Vec<_>>(),
            vec![
                crate::client_semantics::SemanticsStatus::Answered,
                crate::client_semantics::SemanticsStatus::Answered,
                crate::client_semantics::SemanticsStatus::Pending,
                crate::client_semantics::SemanticsStatus::Skipped,
            ]
        );
        assert_eq!(
            persisted["client_semantics"][3]["major"],
            serde_json::Value::Null,
            "an unknown major is written as null, as the wire writes it"
        );
    }

    #[test]
    fn vite_artifacts_are_excluded_from_service_and_include_roots() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        for directory in ["apps/api", "shared"] {
            for subtree in ["src", ".vite/deps", "src/.vite/deps", "src/.vite-tools"] {
                let dir = root.join(directory).join(subtree);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("client.js"),
                    "fetch('https://vendor.example/api');",
                )
                .unwrap();
            }
        }
        let service = Config {
            directory: Some("apps/api".to_string()),
            include: vec!["shared".to_string()],
            ..Default::default()
        };
        let (files, _) = crate::file_finder::find_service_files(
            root.to_str().unwrap(),
            &service,
            &service_ignore_patterns(&service),
        );
        let mut relative: Vec<_> = files
            .iter()
            .map(|file| file.strip_prefix(root).unwrap().to_path_buf())
            .collect();
        relative.sort();
        assert_eq!(
            relative,
            vec![
                PathBuf::from("apps/api/src/.vite-tools/client.js"),
                PathBuf::from("apps/api/src/client.js"),
                PathBuf::from("shared/src/.vite-tools/client.js"),
                PathBuf::from("shared/src/client.js"),
            ]
        );
    }

    /// The deterministic candidate scan reaches the payload, and it reaches it
    /// as its own channel: endpoints and calls are untouched, so SDK rows can
    /// never leak into HTTP endpoint matching.
    #[test]
    fn attach_external_call_candidates_populates_only_the_new_field() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-call-candidates");
        let fixture_str = fixture.to_string_lossy().to_string();
        let service = Config {
            directory: Some("apps/api".to_string()),
            ..Default::default()
        };
        let ignore_patterns = service_ignore_patterns(&service);
        let (files, package_json) =
            crate::file_finder::find_service_files(&fixture_str, &service, &ignore_patterns);
        let mut packages = Packages::new(package_json.into_iter().collect()).expect("packages");
        packages.internal_names = crate::packages::collect_internal_package_names(&fixture);

        let mut data = CloudRepoData {
            repo_name: "svc".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        attach_external_call_candidates(
            &mut data,
            &fixture_str,
            &files,
            &service,
            &mut crate::external_call_candidates::WorkspaceScan::new(),
        );

        let rows = data
            .external_call_candidates
            .as_ref()
            .expect("fixture yields candidates");
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .all(|row| row.mechanism == crate::external_call_candidates::CallMechanism::Sdk)
        );
        assert!(data.endpoints.is_empty(), "endpoints must not be touched");
        assert!(data.calls.is_empty(), "calls must not be touched");
    }

    /// Both sources reach the payload as one list, and the HTTP rows come from
    /// the mount graph the payload itself carries — the same calls the index
    /// records, not a second traversal that could disagree with it.
    #[test]
    fn attach_external_call_candidates_merges_sdk_and_http_rows() {
        use crate::external_call_candidates::CallMechanism;

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-call-candidates");
        let fixture_str = fixture.to_string_lossy().to_string();
        let service = Config {
            directory: Some("apps/api".to_string()),
            external_domains: ["api.vendor.test".to_string()].into_iter().collect(),
            external_env_vars: ["BILLING_API".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let ignore_patterns = service_ignore_patterns(&service);
        let (files, package_json) =
            crate::file_finder::find_service_files(&fixture_str, &service, &ignore_patterns);
        let mut packages = Packages::new(package_json.into_iter().collect()).expect("packages");
        packages.internal_names = crate::packages::collect_internal_package_names(&fixture);

        let mut graph = crate::mount_graph::MountGraph::new();
        graph.data_calls = vec![
            crate::mount_graph::DataFetchingCall {
                method: "POST".to_string(),
                target_url: "https://api.vendor.test/v1/charges".to_string(),
                canonical_path: "/v1/charges".to_string(),
                client: "fetch(".to_string(),
                file_location: format!("{}/apps/api/src/pay.ts:12", fixture_str),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: Some("api.vendor.test".to_string()),
                line: Some(12),
                base: None,
                consumers_not_resolved: None,
                resolution_source: None,
                dispatch: None,
                role: None,
                reaches_request: None,
                library_semantics: Vec::new(),
            },
            crate::mount_graph::DataFetchingCall {
                method: "GET".to_string(),
                target_url: "${process.env.BILLING_API}/invoices".to_string(),
                canonical_path: "${process.env.BILLING_API}/invoices".to_string(),
                client: "axios.".to_string(),
                file_location: "apps/api/src/billing.ts:7".to_string(),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: None,
                line: Some(7),
                base: None,
                consumers_not_resolved: None,
                resolution_source: None,
                dispatch: None,
                role: None,
                reaches_request: None,
                library_semantics: Vec::new(),
            },
        ];

        let mut data = CloudRepoData {
            repo_name: "svc".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: Some(graph),
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        attach_external_call_candidates(
            &mut data,
            &fixture_str,
            &files,
            &service,
            &mut crate::external_call_candidates::WorkspaceScan::new(),
        );

        let rows = data.external_call_candidates.expect("rows");
        assert!(
            rows.iter().any(|row| row.mechanism == CallMechanism::Sdk),
            "SDK rows survive the merge: {:?}",
            rows
        );
        let http: Vec<_> = rows
            .iter()
            .filter(|row| row.mechanism != CallMechanism::Sdk)
            .map(|row| {
                (
                    row.file.as_str(),
                    row.line,
                    row.callee.as_str(),
                    row.package.as_str(),
                    row.mechanism,
                )
            })
            .collect();
        assert_eq!(
            http,
            vec![
                (
                    "apps/api/src/billing.ts",
                    7,
                    "GET",
                    "BILLING_API",
                    CallMechanism::EnvVarUrl
                ),
                (
                    "apps/api/src/pay.ts",
                    12,
                    "POST",
                    "api.vendor.test",
                    CallMechanism::ExternalHttp
                ),
            ],
            "an absolute scan-root prefix is stripped, so both paths key the same way"
        );
        // The whole list is sorted as one set, whatever produced each row.
        let mut sorted = rows.clone();
        sorted.sort();
        assert_eq!(rows, sorted);
    }

    #[test]
    fn test_function_definition_intent_hash_roundtrips() {
        // The content-hash cache only works if `intent` and `intent_input_hash`
        // survive the upload/download JSON round-trip. A silently-dropped hash
        // would turn every incremental scan into a full cache miss.
        let mut function_definitions = HashMap::new();
        function_definitions.insert(
            "getUser".to_string(),
            crate::visitor::FunctionDefinition {
                name: "getUser".to_string(),
                file_path: "src/users.ts".into(),
                node_type: Default::default(),
                arguments: vec![],
                body_source: None, // stripped before upload
                is_exported: true,
                line_number: 1,
                end_line: 0,
                intent: Some("fetches a user by id".to_string()),
                calls: vec![],
                tokens: vec![],
                return_type: None,
                return_is_explicit: false,
                signature: None,
                intent_input_hash: Some("deadbeef".to_string()),
                dispatch_table: None,
            },
        );

        let data = CloudRepoData {
            repo_name: "svc".to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions,
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: Some(CACHE_VERSION),
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        };

        let json = serde_json::to_string(&data).expect("should serialize");
        let deserialized: CloudRepoData = serde_json::from_str(&json).expect("should deserialize");

        let def = &deserialized.function_definitions["getUser"];
        assert_eq!(def.intent.as_deref(), Some("fetches a user by id"));
        assert_eq!(def.intent_input_hash.as_deref(), Some("deadbeef"));

        // And the map feeding the cache is rebuilt correctly from that blob.
        let previous = PreviousIntents::from_definitions(&deserialized.function_definitions);
        assert_eq!(previous.for_hash("deadbeef"), Some("fetches a user by id"));
    }

    #[test]
    fn test_cloud_repo_data_without_cache_fields_deserializes() {
        // Old CloudRepoData without cache fields should still deserialize (backwards compat)
        let json = r#"{
            "repo_name": "old-repo",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "last_updated": "2025-01-01T00:00:00Z",
            "commit_hash": "old123"
        }"#;

        let data: CloudRepoData =
            serde_json::from_str(json).expect("should deserialize old format");
        assert_eq!(data.repo_name, "old-repo");
        assert!(data.file_results.is_none());
        assert!(data.cached_detection.is_none());
        assert!(data.cached_guidance.is_none());
        assert!(data.package_json_hash.is_none());
        assert!(data.cache_version.is_none());
    }

    #[test]
    fn resolve_services_without_config_defaults_to_single_service() {
        let tmp = tempfile::tempdir().unwrap();
        let services = resolve_services(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(services.len(), 1);
        assert!(services[0].directory.is_none());
    }

    #[test]
    fn resolve_services_rejects_malformed_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("carrick.json"), "{ not valid json").unwrap();

        let err = resolve_services(tmp.path().to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("Failed to parse"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn resolve_services_rejects_unreferenced_includes_key() {
        // Reported as itself, not wrapped in the parse-failure advice.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("lambdas/api")).unwrap();
        std::fs::create_dir_all(tmp.path().join("lambdas/_shared")).unwrap();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{
                "includes": { "lambdas/_shard": { "externalEnvVars": ["GITHUB_API_BASE"] } },
                "services": [
                    { "name": "api", "directory": "lambdas/api", "include": ["lambdas/_shared"] }
                ]
            }"#,
        )
        .unwrap();

        let err = resolve_services(tmp.path().to_str().unwrap()).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("lambdas/_shard"), "{message}");
        assert!(
            !message.contains("Failed to parse"),
            "a semantic error should not be reported as a parse failure: {message}"
        );
    }

    #[test]
    fn resolve_services_inherits_include_root_declarations() {
        // The whole engine path: parse, inherit, validate declared paths exist.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("lambdas/api")).unwrap();
        std::fs::create_dir_all(tmp.path().join("lambdas/worker")).unwrap();
        std::fs::create_dir_all(tmp.path().join("lambdas/_shared")).unwrap();
        std::fs::create_dir_all(tmp.path().join("app")).unwrap();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{
                "includes": {
                    "lambdas/_shared": { "externalEnvVars": ["GITHUB_API_BASE"] }
                },
                "services": [
                    { "name": "api", "directory": "lambdas/api", "include": ["lambdas/_shared"] },
                    { "name": "worker", "directory": "lambdas/worker", "include": ["lambdas/_shared"] },
                    { "name": "web", "directory": "app" }
                ]
            }"#,
        )
        .unwrap();

        let services = resolve_services(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(services.len(), 3);
        assert!(services[0].is_external_call("ENV_VAR:GITHUB_API_BASE:/repos"));
        assert!(services[1].is_external_call("ENV_VAR:GITHUB_API_BASE:/repos"));
        assert!(!services[2].is_external_call("ENV_VAR:GITHUB_API_BASE:/repos"));
    }

    #[test]
    fn resolve_services_rejects_missing_service_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{ "services": [{ "name": "api", "directory": "does-not-exist" }] }"#,
        )
        .unwrap();

        let err = resolve_services(tmp.path().to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("does-not-exist")
                && err.to_string().contains("does not exist"),
            "expected missing-directory error, got: {err}"
        );
    }

    #[test]
    fn resolve_services_rejects_missing_include_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("svc")).unwrap();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{ "services": [{ "name": "api", "directory": "svc", "include": ["shared"] }] }"#,
        )
        .unwrap();

        let err = resolve_services(tmp.path().to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("include path 'shared'"),
            "expected missing-include error, got: {err}"
        );
    }

    #[test]
    fn resolve_services_accepts_valid_service_paths() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("svc")).unwrap();
        std::fs::create_dir(tmp.path().join("shared")).unwrap();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{ "services": [{ "name": "api", "directory": "svc", "include": ["shared"] }] }"#,
        )
        .unwrap();

        let services = resolve_services(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].directory.as_deref(), Some("svc"));
    }

    #[test]
    fn load_packages_rejects_malformed_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{ trailing-comma: ,}").unwrap();

        let err = load_packages_for_service(tmp.path().to_str().unwrap(), &Config::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("Failed to parse"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn load_packages_missing_package_json_defaults_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let packages =
            load_packages_for_service(tmp.path().to_str().unwrap(), &Config::default()).unwrap();
        assert!(packages.merged_dependencies.is_empty());
    }

    #[test]
    fn discovery_rejects_service_with_no_source_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cm: Lrc<SourceMap> = Default::default();

        let err = discover_files_and_symbols(tmp.path().to_str().unwrap(), &Config::default(), cm)
            .unwrap_err();
        assert!(
            err.to_string().contains("No JS/TS source files"),
            "expected empty-scan error, got: {err}"
        );
    }

    /// A CommonJS service states every package it loads to framework
    /// detection (carrick#1727), which classifies only the packages the
    /// import list names. Every module here is loaded by `require` or
    /// `import()`, none by an import declaration, so before the fix the list
    /// was empty. The computed `require` names no module and adds nothing.
    #[test]
    fn discovery_samples_the_modules_a_commonjs_service_loads() {
        let repo = format!(
            "{}/tests/fixtures/commonjs-import-sample",
            env!("CARGO_MANIFEST_DIR")
        );
        let cm: Lrc<SourceMap> = Default::default();
        let sample = discover_files_and_symbols(&repo, &Config::default(), cm)
            .unwrap()
            .import_facts;
        assert_eq!(
            sample.statements(),
            vec![
                "import { publishOrder } from './publisher';",
                "import 'dotenv';",
                "import * as express from 'express';",
                "import 'ioredis';",
                "import { Kafka } from 'kafkajs';",
                "import 'pino';",
            ]
        );
    }

    #[test]
    fn discovery_keeps_imported_class_private_members_inaccessible() {
        let repo = format!(
            "{}/tests/fixtures/member-accessibility",
            env!("CARGO_MANIFEST_DIR")
        );
        let cm: Lrc<SourceMap> = Default::default();
        let definitions = discover_files_and_symbols(&repo, &Config::default(), cm)
            .unwrap()
            .function_definitions;
        for name in ["Surface.run", "Surface.create", "consume"] {
            assert!(definitions[name].is_exported, "{name} is public");
        }
        for name in [
            "Surface.hidden",
            "Surface.inherited",
            "Surface.#secret",
            "Surface.reset",
            "Surface.state",
            "Surface.action",
            "Surface.#privateAction",
        ] {
            assert!(
                !definitions[name].is_exported,
                "importing Surface must not promote {name}"
            );
        }
        assert!(
            definitions["Surface.run"]
                .calls
                .iter()
                .any(|call| call.name == "Surface.hidden"),
            "private members remain indexed and callable within the class"
        );
    }

    /// Discovery resolves call edges on a real checkout, not just in the
    /// resolver's own unit tests. `Ledger.scrape` (src/index.ts:27) calls
    /// `auditLog`, imported from `./util/audit.js` — so this covers the whole
    /// production path: the walked file list, the canonical-path keying the
    /// import lookup depends on, and the NodeNext `.js` → `.ts` rewrite.
    /// Unit tests over hand-built maps cannot tell "resolution works" apart
    /// from "resolution silently returns nothing here".
    #[test]
    fn discovery_resolves_call_edges_on_a_real_fixture() {
        let repo = format!("{}/tests/fixtures/sdk-surface", env!("CARGO_MANIFEST_DIR"));
        let cm: Lrc<SourceMap> = Default::default();

        let definitions = discover_files_and_symbols(&repo, &Config::default(), cm)
            .unwrap()
            .function_definitions;

        let scrape = definitions
            .get("Ledger.scrape")
            .expect("Ledger.scrape indexed");
        let audit = scrape
            .calls
            .iter()
            .find(|c| c.name == "auditLog")
            .unwrap_or_else(|| panic!("no auditLog edge, got {:?}", scrape.calls));
        assert!(
            audit.file_path.ends_with("src/util/audit.ts"),
            "edge must point at the imported module, got {}",
            audit.file_path
        );
        assert_eq!(audit.line_number, 1, "auditLog is defined on line 1");
        assert_eq!(audit.call_site_line, 27, "called on line 27 of index.ts");
        assert!(
            !scrape.calls.iter().any(|c| c.name == "chargeCard"),
            "chargeCard is imported but never called by scrape, got {:?}",
            scrape.calls
        );
    }

    /// carrick#679: a call through a name the entry publishes with `export *
    /// as vaults from "./vaults.js"`. The receiver is a namespace one hop
    /// away, so nothing in the calling file imports the module that declares
    /// the function — `refresh` had no edge at all until the export table
    /// carried the form.
    #[test]
    fn discovery_resolves_a_call_through_a_namespace_reexport() {
        let repo = format!("{}/tests/fixtures/sdk-surface", env!("CARGO_MANIFEST_DIR"));
        let cm: Lrc<SourceMap> = Default::default();

        let definitions = discover_files_and_symbols(&repo, &Config::default(), cm)
            .unwrap()
            .function_definitions;

        let refresh = definitions.get("refresh").expect("refresh indexed");
        let list = refresh
            .calls
            .iter()
            .find(|c| c.name == "list")
            .unwrap_or_else(|| panic!("no `list` edge, got {:?}", refresh.calls));
        assert!(
            list.file_path.ends_with("src/vaults.ts"),
            "the edge must point at the module the namespace names, got {}",
            list.file_path
        );
    }

    /// The re-keying #582 introduced happens inside discovery, so the file
    /// qualifier has to be repo-RELATIVE on the real path shapes discovery
    /// sees: `repo_path` as the caller typed it (here a temp dir, which is a
    /// symlink on macOS) and file paths as the walker yields them. An absolute
    /// path in a key would ship a CI checkout prefix to the index, and
    /// `relativize_function_definition_paths` rewrites values, never keys, so
    /// nothing downstream would strip it.
    /// Write `files` under a temp dir and run discovery over it.
    fn discover_sources(files: &[(&str, &str)]) -> (tempfile::TempDir, FileDiscovery) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for (name, source) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
            std::fs::write(&path, source).expect("write");
        }
        let cm: Lrc<SourceMap> = Default::default();
        let discovery =
            discover_files_and_symbols(&dir.path().to_string_lossy(), &Config::default(), cm)
                .unwrap();
        (dir, discovery)
    }

    /// The summaries composed from discovery, with no library semantics.
    fn summaries_of(discovery: &FileDiscovery) -> crate::request_summary::RequestSummaryIndex {
        crate::request_summary::summarize(
            &discovery.request_inputs,
            &crate::client_semantics::LibrarySemantics::default(),
        )
    }

    /// The summary rows discovery states for `file`, flattened in site order.
    fn summary_rows_of(
        dir: &tempfile::TempDir,
        discovery: &FileDiscovery,
        file: &str,
    ) -> Vec<crate::request_summary::SummaryRow> {
        summaries_of(discovery)
            .rows(&dir.path().join(file))
            .map(|sites| sites.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    /// carrick#1555: a transport helper handed its URL and its options states
    /// the request where a caller fills them, and a call through the client in
    /// another module reaches THAT line — the row that states the request —
    /// not the helper's own `fetch`, which states none.
    #[test]
    fn a_request_is_stated_where_its_last_hole_is_filled() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/send.ts",
                "export async function send(url: string, options: { method: string }) {\n  const response = await fetch(url, options);\n  return response.json();\n}\n",
            ),
            (
                "src/client.ts",
                "import { send } from \"./send\";\n\nexport class Client {\n  constructor(private readonly baseUrl: string) {}\n\n  read(id: string) {\n    return send(`${this.baseUrl}/api/v1/widgets/${id}`, { method: \"GET\" });\n  }\n}\n",
            ),
            (
                "src/use.ts",
                "import type { Client } from \"./client\";\n\nexport async function go(client: Client, widget: string) {\n  return client.read(widget);\n}\n",
            ),
        ]);

        assert!(
            summary_rows_of(&dir, &discovery, "src/send.ts").is_empty(),
            "the helper's own fetch states no URL"
        );

        let stated = summary_rows_of(&dir, &discovery, "src/client.ts");
        assert_eq!(stated.len(), 1, "{stated:#?}");
        assert_eq!(stated[0].line, 7);
        assert_eq!(stated[0].method, "GET");
        assert_eq!(stated[0].target, "${this.baseUrl}/api/v1/widgets/${id}");
        assert_eq!(stated[0].reaches_request, None, "this line IS the request");

        let through = summary_rows_of(&dir, &discovery, "src/use.ts");
        assert_eq!(through.len(), 1, "{through:#?}");
        assert_eq!(through[0].line, 4);
        assert_eq!(through[0].method, "GET");
        assert_eq!(
            through[0].target, "${this.baseUrl}/api/v1/widgets/${id}",
            "the path parameter keeps the client's own name"
        );
        let reaches = through[0].reaches_request.as_deref().unwrap_or_default();
        assert!(
            reaches.ends_with("src/client.ts:7"),
            "reaches the line that states the request, got {reaches}"
        );
    }

    /// carrick#1555: a URL written inline at the request is the site passes'
    /// to read; a URL a field holds is stated by the summary at the request's
    /// own line.
    #[test]
    fn a_request_states_its_own_line_only_for_a_url_read_through_a_binding() {
        let (dir, discovery) = discover_sources(&[(
            "src/client.ts",
            "const BASE = process.env.API_URL;\n\nexport async function inline() {\n  return fetch(`${BASE}/inline`, { method: \"GET\" });\n}\n\nexport class Gateway {\n  private url: string;\n  constructor(endpoint: string) {\n    this.url = `${endpoint}/rpc`;\n  }\n  call() {\n    return fetch(this.url, { method: \"POST\", body: JSON.stringify({ op: \"ping\" }) });\n  }\n}\n",
        )]);

        let rows = summary_rows_of(&dir, &discovery, "src/client.ts");
        assert_eq!(rows.len(), 1, "only the field-held URL: {rows:#?}");
        assert_eq!(rows[0].line, 13);
        assert_eq!(rows[0].method, "POST");
        assert_eq!(rows[0].target, "${endpoint}/rpc");
        assert_eq!(
            rows[0].body_literals.get("op").map(String::as_str),
            Some("ping")
        );
        assert!(rows[0].own_site);
    }

    /// carrick#1648: a name is read as the binding the identifier resolves
    /// to. A block that declares a module constant, a local or a parameter
    /// again holds a value of its own, which this pass does not read, so the
    /// call there states nothing; the outer binding is still read where it is
    /// the one named.
    #[test]
    fn a_name_a_block_declares_again_is_never_read_as_the_outer_binding() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/load.ts",
                "const USERS = \"/api/users\";\n\nexport async function load(admin: boolean) {\n  if (admin) {\n    const USERS = \"/api/admins\";\n    return fetch(USERS, { method: \"POST\" });\n  }\n  return null;\n}\n\nexport async function loadAll() {\n  return fetch(USERS, { method: \"PUT\" });\n}\n",
            ),
            (
                "src/local.ts",
                "export async function local(admin: boolean, path: string) {\n  const ITEMS = \"/api/items\";\n  if (admin) {\n    const ITEMS = \"/api/admin-items\";\n    const path = \"/api/admin-path\";\n    await fetch(path, { method: \"DELETE\" });\n    return fetch(ITEMS, { method: \"POST\" });\n  }\n  return fetch(ITEMS, { method: \"GET\" });\n}\n",
            ),
            (
                "src/use.ts",
                "import { local } from \"./local\";\n\nexport async function go() {\n  return local(true, \"/api/caller-path\");\n}\n",
            ),
        ]);

        let rows = |file: &str| -> Vec<(u32, String, String)> {
            summary_rows_of(&dir, &discovery, file)
                .into_iter()
                .map(|row| (row.line, row.method, row.target))
                .collect()
        };
        assert_eq!(
            rows("src/load.ts"),
            vec![(12, "PUT".to_string(), "/api/users".to_string())],
            "the module constant only where it is the binding named"
        );
        assert_eq!(
            rows("src/local.ts"),
            vec![(9, "GET".to_string(), "/api/items".to_string())],
            "the function's local only where it is the binding named"
        );
        assert_eq!(
            rows("src/use.ts"),
            vec![(4, "GET".to_string(), "/api/items".to_string())],
            "the request the callee states, and none filled by the caller's \
             argument: the block declared that parameter again"
        );
    }

    /// carrick#1555: a site is recorded as sending nothing only when its callee
    /// provably sends nothing. A body with no call is proof; a declaration with
    /// no body, or a construction, is not.
    #[test]
    fn a_site_is_silent_only_when_its_callee_is_proven_to_send_nothing() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/counter.ts",
                "declare function sendMetric(name: string): void;\n\nexport class Counter {\n  private count = 0;\n  bump(): void {\n    this.count += 1;\n  }\n  report(): void {\n    sendMetric(\"count\");\n  }\n  start(): void {\n    new Worker(\"poll.js\");\n  }\n}\n",
            ),
            (
                "src/use.ts",
                "import type { Counter } from \"./counter\";\n\nexport function tick(counter: Counter) {\n  counter.bump();\n  counter.report();\n  counter.start();\n}\n",
            ),
        ]);

        let silent = summaries_of(&discovery)
            .silent(&dir.path().join("src/use.ts"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            silent.len(),
            1,
            "only `bump()` is proven silent: {silent:?}"
        );
    }

    /// The (line, method, target) of every summary row `file` states.
    fn rows_by_line(
        dir: &tempfile::TempDir,
        discovery: &FileDiscovery,
        file: &str,
    ) -> Vec<(u32, String, String)> {
        summary_rows_of(dir, discovery, file)
            .into_iter()
            .map(|row| (row.line, row.method, row.target))
            .collect()
    }

    /// carrick#1562: a parameter or a field that holds the platform's
    /// `fetch` unless a caller hands another is `fetch`: a positional or a
    /// destructured default, a constructor parameter's default kept in a
    /// field, and a `??` falling back to it. One the function assigns again,
    /// one written in a method, and a parameter with no default (the
    /// caller's own function, composed where the caller writes it) are not.
    /// No call here carries an options bag, which reads as a request
    /// whatever its callee.
    #[test]
    fn an_injected_fetch_is_the_platform_s_fetch() {
        let (dir, discovery) = discover_sources(&[(
            "src/client.ts",
            "declare function wrap(f: typeof fetch): typeof fetch;\n\
             const BASE = process.env.API_BASE;\n\
             export async function post(owner: string, { fetchImpl = fetch }: { fetchImpl?: typeof fetch }) {\n\
             \x20 const url = `${BASE}/repos/${owner}/comments`;\n\
             \x20 return fetchImpl(url);\n\
             }\n\
             export async function get(id: string, fetchFn: typeof fetch = fetch) {\n\
             \x20 const url = `${BASE}/items/${id}`;\n\
             \x20 return fetchFn(url);\n\
             }\n\
             export async function swapped(id: string, fetchFn: typeof fetch = fetch) {\n\
             \x20 fetchFn = wrap(fetchFn);\n\
             \x20 const url = `${BASE}/swapped/${id}`;\n\
             \x20 return fetchFn(url);\n\
             }\n\
             export async function handed(id: string, fetchFn: typeof fetch) {\n\
             \x20 const url = `${BASE}/handed/${id}`;\n\
             \x20 return fetchFn(url);\n\
             }\n\
             export class Client {\n\
             \x20 #fetch: typeof fetch;\n\
             \x20 private fetchFn: typeof fetch;\n\
             \x20 constructor(fetcher: typeof fetch = fetch, options: { fetch?: typeof fetch } = {}) {\n\
             \x20   this.#fetch = fetcher;\n\
             \x20   this.fetchFn = options.fetch ?? fetch;\n\
             \x20 }\n\
             \x20 ping() { const url = `${BASE}/ping`; return this.#fetch(url); }\n\
             \x20 pong() { const url = `${BASE}/pong`; return this.fetchFn(url); }\n\
             }\n\
             export class Swapped {\n\
             \x20 #fetch: typeof fetch = fetch;\n\
             \x20 swap(other: typeof fetch) { this.#fetch = other; }\n\
             \x20 ping() { const url = `${BASE}/moved`; return this.#fetch(url); }\n\
             }\n",
        )]);
        let get = |target: &str| (String::from("GET"), target.to_string());
        let rows: Vec<(u32, (String, String))> = rows_by_line(&dir, &discovery, "src/client.ts")
            .into_iter()
            .map(|(line, method, target)| (line, (method, target)))
            .collect();
        assert_eq!(
            rows,
            vec![
                (5, get("${process.env.API_BASE}/repos/${owner}/comments")),
                (9, get("${process.env.API_BASE}/items/${id}")),
                (27, get("${process.env.API_BASE}/ping")),
                (28, get("${process.env.API_BASE}/pong")),
            ]
        );
    }

    /// carrick#1601: a row the summaries state at a call to a function the
    /// service declares is restated at that caller, and says so, wherever the
    /// function is: in the same module (the issue's token exchange, where the
    /// caller fills the URL's holes) or in another (a helper handed the
    /// platform's `fetch` by default, called with its base). The function's
    /// own request line is no caller.
    #[test]
    fn a_row_stated_at_a_helper_s_caller_is_marked_as_restated_there() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/tools.ts",
                "async function exchangeToken(origin: string, token: string, account: string): Promise<string | null> {\n\
                 \x20 const res = await fetch(`${origin}/api/v1/accounts/${account}/token`, {\n\
                 \x20   method: \"POST\",\n\
                 \x20   headers: { Authorization: `Bearer ${token}` },\n\
                 \x20   body: JSON.stringify({ scopes: [\"read\"] }),\n\
                 \x20 });\n\
                 \x20 if (!res.ok) return null;\n\
                 \x20 const data = (await res.json()) as { token?: string };\n\
                 \x20 return data.token ?? null;\n\
                 }\n\
                 export function buildTools(ctx: { token: string; account: string }) {\n\
                 \x20 const origin = process.env.API_ORIGIN;\n\
                 \x20 let pending: Promise<string | null> | undefined;\n\
                 \x20 function getToken(): Promise<string | null> {\n\
                 \x20   pending ??= exchangeToken(origin, ctx.token, ctx.account);\n\
                 \x20   return pending;\n\
                 \x20 }\n\
                 \x20 return { getToken };\n\
                 }\n",
            ),
            (
                "src/availability.ts",
                "type Params = { id: string; apiUrl: string; fetchFn?: typeof fetch };\n\
                 export const checkAvailability = async ({ id, apiUrl, fetchFn = fetch }: Params) => {\n\
                 \x20 const response = await fetchFn(`${apiUrl}/things/${encodeURIComponent(id)}/availability`);\n\
                 \x20 if (!response.ok) throw new Error(`HTTP ${response.status}`);\n\
                 \x20 const data = (await response.json()) as { available: boolean };\n\
                 \x20 return data.available === true;\n\
                 };\n",
            ),
            (
                "src/page.ts",
                "import { checkAvailability } from \"./availability\";\n\
                 const API_URL = process.env.API_URL;\n\
                 export function onBlur(id: string) {\n\
                 \x20 return checkAvailability({ id, apiUrl: API_URL });\n\
                 }\n",
            ),
        ]);
        let marks = |file: &str| -> Vec<(u32, String, bool, bool)> {
            summary_rows_of(&dir, &discovery, file)
                .into_iter()
                .map(|row| (row.line, row.method, row.at_caller, row.own_site))
                .collect()
        };
        assert_eq!(
            marks("src/tools.ts"),
            vec![(15, "POST".to_string(), true, false)],
            "the token exchange is stated at the caller that fills its holes, and is marked so"
        );
        assert_eq!(
            marks("src/page.ts"),
            vec![(4, "GET".to_string(), true, false)],
            "the availability check is stated at its caller in another module, and is marked so"
        );
        assert!(
            marks("src/availability.ts")
                .iter()
                .all(|(_, _, at_caller, _)| !at_caller),
            "the helper's own request line is no caller: {:?}",
            marks("src/availability.ts")
        );
    }

    /// carrick#1601: a call of a function that makes one request and hands
    /// back its parsed body unchanged is worth that body, so a row stated
    /// there is not marked: a generic transport helper (`send<T>`), the body
    /// held in a local first, the response parsed where it is awaited, an
    /// arrow, and a function that returns such a helper's call (`viaSend`). A
    /// helper that hands back the raw response, returns early with something
    /// else, makes two requests, returns a call of a helper that hands back
    /// the raw response (`viaRaw`), or parses a response its request did not
    /// answer with (`stored`) is marked.
    #[test]
    fn a_helper_that_hands_back_its_parsed_body_leaves_its_caller_unmarked() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/send.ts",
                "export async function send<T>(url: string, options: { method: string }): Promise<T> {\n\
                 \x20 const response = await fetch(url, options);\n\
                 \x20 return (await response.json()) as T;\n\
                 }\n\
                 export async function held(url: string) {\n\
                 \x20 const res = await fetch(url, { method: \"GET\" });\n\
                 \x20 const data = (await res.json()) as { id: string };\n\
                 \x20 return data;\n\
                 }\n\
                 export async function inline(url: string) {\n\
                 \x20 return (await fetch(url, { method: \"GET\" })).json();\n\
                 }\n\
                 export const arrow = async (url: string) => (await fetch(url, { method: \"GET\" })).json();\n\
                 export async function raw(url: string) {\n\
                 \x20 const res = await fetch(url, { method: \"GET\" });\n\
                 \x20 return res;\n\
                 }\n\
                 export async function early(url: string) {\n\
                 \x20 const res = await fetch(url, { method: \"GET\" });\n\
                 \x20 if (!res.ok) return null;\n\
                 \x20 return res.json();\n\
                 }\n\
                 export async function twice(url: string) {\n\
                 \x20 await fetch(`${url}/audit`, { method: \"POST\" });\n\
                 \x20 const res = await fetch(url, { method: \"GET\" });\n\
                 \x20 return res.json();\n\
                 }\n\
                 export async function viaSend(url: string) {\n\
                 \x20 return send<{ id: string }>(url, { method: \"GET\" });\n\
                 }\n\
                 export async function viaRaw(url: string) {\n\
                 \x20 return raw(url);\n\
                 }\n\
                 declare const cache: { read(key: string): Promise<Response> };\n\
                 export async function stored(url: string) {\n\
                 \x20 await fetch(url, { method: \"GET\" });\n\
                 \x20 return (await cache.read(url)).json();\n\
                 }\n",
            ),
            (
                "src/client.ts",
                "import { send, held, inline, arrow, raw, early, twice, viaSend, viaRaw, stored } from \"./send\";\n\
                 const BASE = process.env.API_BASE;\n\
                 export function readWidget(id: string) {\n\
                 \x20 return send<{ id: string }>(`${BASE}/widgets/${id}`, { method: \"GET\" });\n\
                 }\n\
                 export function readAll() {\n\
                 \x20 held(`${BASE}/held`);\n\
                 \x20 inline(`${BASE}/inline`);\n\
                 \x20 arrow(`${BASE}/arrow`);\n\
                 \x20 raw(`${BASE}/raw`);\n\
                 \x20 early(`${BASE}/early`);\n\
                 \x20 twice(`${BASE}/twice`);\n\
                 \x20 viaSend(`${BASE}/via-send`);\n\
                 \x20 viaRaw(`${BASE}/via-raw`);\n\
                 \x20 stored(`${BASE}/stored`);\n\
                 }\n",
            ),
        ]);
        let mut marks: Vec<(u32, String, bool)> =
            summary_rows_of(&dir, &discovery, "src/client.ts")
                .into_iter()
                .map(|row| {
                    let last = row
                        .target
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    (row.line, last, row.at_caller)
                })
                .collect();
        marks.sort();
        assert_eq!(
            marks,
            vec![
                (4, "${id}".to_string(), false),
                (7, "held".to_string(), false),
                (8, "inline".to_string(), false),
                (9, "arrow".to_string(), false),
                (10, "raw".to_string(), true),
                (11, "early".to_string(), true),
                (12, "audit".to_string(), true),
                (12, "twice".to_string(), true),
                (13, "via-send".to_string(), false),
                (14, "via-raw".to_string(), true),
                (15, "stored".to_string(), true),
            ]
        );
    }

    /// carrick#1801: the summaries answer whether a function hands back its
    /// parsed body for every function they read, by the file's canonical path
    /// (the temp dir is reached through a link on macOS), including one no
    /// call in the service reaches: a caller only the model states is read
    /// against its callee by the module graph, not the call graph.
    #[test]
    fn the_summaries_say_which_functions_hand_back_their_parsed_body() {
        let (dir, discovery) = discover_sources(&[(
            "src/send.ts",
            "export async function held(url: string) {\n\
             \x20 const res = await fetch(url, { method: \"GET\" });\n\
             \x20 const data = (await res.json()) as { id: string };\n\
             \x20 return data;\n\
             }\n\
             export const arrow = async (url: string) => (await fetch(url, { method: \"GET\" })).json();\n\
             export async function raw(url: string) {\n\
             \x20 return fetch(url, { method: \"GET\" });\n\
             }\n\
             export async function mapped(url: string) {\n\
             \x20 const res = await fetch(url, { method: \"GET\" });\n\
             \x20 const data = (await res.json()) as { id: string };\n\
             \x20 return { id: data.id };\n\
             }\n",
        )]);
        let summaries = summaries_of(&discovery);
        let file = dir.path().join("src/send.ts").canonicalize().unwrap();
        let passes: Vec<(&str, bool)> = ["held", "arrow", "raw", "mapped", "missing"]
            .into_iter()
            .map(|name| (name, summaries.passes_body(&file, name)))
            .collect();
        assert_eq!(
            passes,
            vec![
                ("held", true),
                ("arrow", true),
                ("raw", false),
                ("mapped", false),
                ("missing", false),
            ]
        );
    }

    /// carrick#1782: a row stated at a call to a function the service
    /// declares says what the call's body is, from what the function does
    /// with its parameters.
    ///
    /// - It sends a declared parameter unchanged (`create`, and `viaCreate`,
    ///   which hands its own parameter on): the body is read at THAT
    ///   function's declaration, the one the site calls.
    /// - It builds its body (`rename`), sends none (`remove`), or assigns the
    ///   parameter again before sending it (`stamped`, and `viaStamped`, one
    ///   call further out): the site states no body.
    /// - Its parameter's declaration states nothing (`loose`), or it is a
    ///   transport the caller hands a path (`post`): no fact, so the site's
    ///   own payload stays the reading.
    #[test]
    fn a_row_at_a_call_says_what_body_the_function_sends() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/api.ts",
                "export type CreateBody = { name: string; mode: \"a\" | \"b\" };\n\
                 const BASE = process.env.API_BASE;\n\
                 export async function create(body: CreateBody) {\n\
                 \x20 const res = await fetch(`${BASE}/v1/things`, { method: \"POST\", body: JSON.stringify(body) });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function rename(id: string, name: string) {\n\
                 \x20 const res = await fetch(`${BASE}/v1/things/${id}`, { method: \"PATCH\", body: JSON.stringify({ name }) });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function loose(body: unknown) {\n\
                 \x20 const res = await fetch(`${BASE}/v1/loose`, { method: \"POST\", body: JSON.stringify(body) });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function stamped(body: CreateBody) {\n\
                 \x20 body = { ...body, name: body.name.trim() };\n\
                 \x20 const res = await fetch(`${BASE}/v1/stamped`, { method: \"POST\", body: JSON.stringify(body) });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function remove(id: string) {\n\
                 \x20 const res = await fetch(`${BASE}/v1/things/${id}`, { method: \"DELETE\" });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function post(path: string, body: CreateBody) {\n\
                 \x20 const res = await fetch(`${BASE}${path}`, { method: \"POST\", body: JSON.stringify(body) });\n\
                 \x20 return res.ok;\n\
                 }\n\
                 export async function viaCreate(input: CreateBody) {\n\
                 \x20 return create(input);\n\
                 }\n\
                 export async function viaStamped(input: CreateBody) {\n\
                 \x20 return stamped(input);\n\
                 }\n",
            ),
            (
                "src/page.ts",
                "import { create, rename, loose, stamped, remove, post, viaCreate, viaStamped } from \"./api\";\n\
                 export async function go(id: string) {\n\
                 \x20 await create({ name: \"x\", mode: \"a\" });\n\
                 \x20 await rename(id, \"y\");\n\
                 \x20 await loose({ any: 1 });\n\
                 \x20 await stamped({ name: \" z \", mode: \"b\" });\n\
                 \x20 await remove(id);\n\
                 \x20 await post(\"/v1/posts\", { name: \"p\", mode: \"a\" });\n\
                 \x20 await viaCreate({ name: \"v\", mode: \"b\" });\n\
                 \x20 await viaStamped({ name: \"w\", mode: \"a\" });\n\
                 }\n",
            ),
        ]);
        let mut bodies: Vec<(u32, String)> = summary_rows_of(&dir, &discovery, "src/page.ts")
            .into_iter()
            .map(|row| {
                let body = match row.call_body {
                    None => "site".to_string(),
                    Some(crate::forwarded_body::CallBody::Built) => "none".to_string(),
                    Some(crate::forwarded_body::CallBody::Param(param)) => {
                        assert!(param.file.ends_with("src/api.ts"), "{}", param.file);
                        format!("declared at {}", param.line)
                    }
                };
                (row.line, body)
            })
            .collect();
        bodies.sort();
        assert_eq!(
            bodies,
            vec![
                (3, "declared at 3".to_string()),
                (4, "none".to_string()),
                (5, "site".to_string()),
                (6, "none".to_string()),
                (7, "none".to_string()),
                (8, "site".to_string()),
                (9, "declared at 28".to_string()),
                (10, "none".to_string()),
            ]
        );
    }

    /// carrick#1562: a call to a module-scope builder is what the builder
    /// returns, with the call's arguments in its parameters: an arrow, a
    /// function declaration, and one held in a constant object. A function
    /// that does more than return, a builder in an object the module writes
    /// through, and one that picks its return by a `switch` (alternatives,
    /// carrick#1694) state nothing.
    #[test]
    fn a_builder_s_return_is_the_request_s_url() {
        let (dir, discovery) = discover_sources(&[(
            "src/api.ts",
            "const BASE = process.env.API_BASE;\n\
             const ENDPOINTS = { users: { list: \"/api/users\", byId: (id: string) => `/api/users/${id}` } };\n\
             const ordersPath = (id: string) => `/api/orders/${id}`;\n\
             function teamPath(team: string) { return `/api/teams/${team}`; }\n\
             function notABuilder(x: string) { const p = `/api/x/${x}`; return p; }\n\
             const MOVED = { path: (id: string) => `/api/m/${id}` };\n\
             MOVED.path = (id: string) => `/elsewhere/${id}`;\n\
             function pathFor(kind: string) {\n\
             \x20 switch (kind) {\n\
             \x20   case \"a\": return \"/api/a\";\n\
             \x20   default: return \"/api/b\";\n\
             \x20 }\n\
             }\n\
             export async function a(id: string) { return fetch(ENDPOINTS.users.byId(id), { method: \"GET\" }); }\n\
             export async function b(id: string) { const url = `${BASE}${ordersPath(id)}`; return fetch(url, { method: \"GET\" }); }\n\
             export async function c() { return fetch(teamPath(\"core\"), { method: \"DELETE\" }); }\n\
             export async function d(x: string) { return fetch(notABuilder(x), { method: \"GET\" }); }\n\
             export async function e(id: string) { return fetch(MOVED.path(id), { method: \"GET\" }); }\n\
             export async function f() { return fetch(pathFor(\"a\"), { method: \"GET\" }); }\n\
             function swappedPath(id: string) { return `/api/swapped/${id}`; }\n\
             export async function g() { return fetch(swappedPath(\"1\"), { method: \"GET\" }); }\n\
             swappedPath = (id: string) => `/elsewhere/${id}`;\n",
        )]);
        assert_eq!(
            rows_by_line(&dir, &discovery, "src/api.ts"),
            vec![
                (14, "GET".to_string(), "/api/users/${id}".to_string()),
                (
                    15,
                    "GET".to_string(),
                    "${process.env.API_BASE}/api/orders/${id}".to_string()
                ),
                (16, "DELETE".to_string(), "/api/teams/core".to_string()),
            ]
        );
    }

    /// The contract sample's semantics (carrick#1564), every claim verified.
    fn verified_sample() -> crate::client_semantics::LibrarySemantics {
        let detection: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .expect("the contract sample parses");
        crate::client_semantics::LibrarySemantics::all_verified(
            &detection.client_semantics.expect("the sample answers"),
        )
    }

    /// Every row the summaries state for `file` with `semantics`.
    fn library_rows_of(
        dir: &tempfile::TempDir,
        discovery: &FileDiscovery,
        file: &str,
        semantics: &crate::client_semantics::LibrarySemantics,
    ) -> Vec<crate::request_summary::SummaryRow> {
        crate::request_summary::summarize(&discovery.request_inputs, semantics)
            .rows(&dir.path().join(file))
            .map(|sites| sites.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    const HTTP_CLIENT: &str = "import http from \"@fixture/http\";\n\
        \n\
        const api = http.create({ baseURL: \"/api/v1\" });\n\
        const edge = http.create({ baseURL: process.env.EDGE_URL, timeout: 500 });\n\
        \n\
        export function listUsers() {\n\
        \x20 return api.get(\"/users\");\n\
        }\n\
        \n\
        export function createOrder() {\n\
        \x20 return api.post(\"/orders\", { action: \"create\" });\n\
        }\n\
        \n\
        export function ping() {\n\
        \x20 return edge.get(\"health\");\n\
        }\n\
        \n\
        export function send(path: string) {\n\
        \x20 return api.get(path);\n\
        }\n\
        \n\
        export function raw() {\n\
        \x20 return http.request({ url: \"/raw\", method: \"post\", data: { op: \"sync\" } });\n\
        }\n\
        \n\
        export function unstated() {\n\
        \x20 return http({ url: \"/unstated\", data: { op: \"x\" } });\n\
        }\n";

    /// carrick#1564: a call through a verified client's instance states its
    /// own row, with the base the factory was handed joined to the path, the
    /// body read through the verified body claim, and the claims it used.
    #[test]
    fn a_verified_instance_joins_its_base_to_the_path_at_its_own_site() {
        let (dir, discovery) = discover_sources(&[("src/api.ts", HTTP_CLIENT)]);
        let rows = library_rows_of(&dir, &discovery, "src/api.ts", &verified_sample());
        let at = |line: u32| -> Vec<&crate::request_summary::SummaryRow> {
            rows.iter().filter(|row| row.line == line).collect()
        };

        let users = at(7);
        assert_eq!(users.len(), 1, "{rows:#?}");
        assert_eq!(users[0].method, "GET");
        assert_eq!(users[0].target, "/api/v1/users");
        assert!(
            !users[0].own_site,
            "claims the site over readings without the base"
        );
        assert_eq!(
            users[0].library_semantics,
            vec![
                "@fixture/http@1:default:factory:create",
                "@fixture/http@1:default:verb:get",
            ]
        );

        let orders = at(11);
        assert_eq!(orders.len(), 1, "{rows:#?}");
        assert_eq!(orders[0].method, "POST");
        assert_eq!(orders[0].target, "/api/v1/orders");
        assert_eq!(
            orders[0].body_literals.get("action").map(String::as_str),
            Some("create")
        );
        assert!(
            orders[0]
                .library_semantics
                .contains(&"@fixture/http@1:default:verb_body:post".to_string()),
            "the body was read through its own claim: {:?}",
            orders[0].library_semantics
        );

        // An opaque base leads, and a path with no slash of its own gets one.
        let health = at(15);
        assert_eq!(health.len(), 1, "{rows:#?}");
        assert_eq!(health[0].target, "${process.env.EDGE_URL}/health");

        // The client itself, handed a config: the method is the literal the
        // config writes, upper-cased, and the body is its data.
        let raw = at(23);
        assert_eq!(raw.len(), 1, "{rows:#?}");
        assert_eq!(
            (raw[0].method.as_str(), raw[0].target.as_str()),
            ("POST", "/raw")
        );
        assert_eq!(
            raw[0].body_literals.get("op").map(String::as_str),
            Some("sync")
        );

        // A config that writes no method states no row: a library's default
        // is never assumed.
        assert!(at(27).is_empty(), "{rows:#?}");
        // A path the caller fills is stated where it is filled, not here.
        assert!(at(19).is_empty(), "{rows:#?}");
    }

    /// carrick#1661: the receiver core reads makers of every form for the
    /// message roles. An instance built any way but `export.member({ … })`
    /// was no client before, and an HTTP reading still reads nothing through
    /// it, whatever the semantics verify. Each form sits in a file of its
    /// own, since one use of the export's binding contests every instance
    /// the file builds from it.
    #[test]
    fn a_maker_only_a_message_role_reads_states_no_http_row() {
        let maker = |init: &str| {
            format!(
                "import http from \"@fixture/http\";\n\
                 const api = {init};\n\
                 export function load() {{ return api.get(\"/users\"); }}\n"
            )
        };
        let files = [
            ("src/control.ts", maker("http.create({ baseURL: \"/v0\" })")),
            (
                "src/constructed.ts",
                maker("new http.create({ baseURL: \"/v1\" })"),
            ),
            ("src/called.ts", maker("http({ baseURL: \"/v2\" })")),
            (
                "src/handed.ts",
                maker("http.create({ baseURL: \"/v3\" }, { retries: 2 })"),
            ),
        ];
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, source)| (*name, source.as_str()))
            .collect();
        let (dir, discovery) = discover_sources(&borrowed);
        let control = library_rows_of(&dir, &discovery, "src/control.ts", &verified_sample());
        assert!(
            control
                .iter()
                .any(|row| row.target == "/v0/users" && !row.library_semantics.is_empty()),
            "the factory form reads through the semantics: {control:#?}"
        );
        for file in ["src/constructed.ts", "src/called.ts", "src/handed.ts"] {
            let verified = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(
                verified.iter().all(|row| row.library_semantics.is_empty()),
                "{file}: {verified:#?}"
            );
            assert_eq!(
                verified,
                library_rows_of(
                    &dir,
                    &discovery,
                    file,
                    &crate::client_semantics::LibrarySemantics::default()
                ),
                "{file}"
            );
        }
    }

    /// carrick#1562: an instance an own factory returns is read by the
    /// message roles ([`crate::request_summary::library_sites`]); an HTTP
    /// reading reads nothing through it, whatever the semantics verify, held
    /// in a module's `const` or a function's. The control beside it is the
    /// same factory call written where it is held.
    #[test]
    fn an_instance_an_own_factory_returns_states_no_http_row() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/factory.ts",
                "import http from \"@fixture/http\";\n\
                 export function makeApi() {\n\
                 \x20 return http.create({ baseURL: \"/v1\" });\n\
                 }\n\
                 const api = makeApi();\n\
                 export function load() { return api.get(\"/users\"); }\n\
                 export function local() { const inner = makeApi(); return inner.get(\"/teams\"); }\n",
            ),
            (
                "src/control.ts",
                "import http from \"@fixture/http\";\n\
                 const direct = http.create({ baseURL: \"/v0\" });\n\
                 export function load() { return direct.get(\"/users\"); }\n",
            ),
        ]);
        let control = library_rows_of(&dir, &discovery, "src/control.ts", &verified_sample());
        assert!(
            control
                .iter()
                .any(|row| row.target == "/v0/users" && !row.library_semantics.is_empty()),
            "the control reads through the semantics: {control:#?}"
        );
        let verified = library_rows_of(&dir, &discovery, "src/factory.ts", &verified_sample());
        assert!(
            verified.iter().all(|row| row.library_semantics.is_empty()),
            "{verified:#?}"
        );
        assert_eq!(
            verified,
            library_rows_of(
                &dir,
                &discovery,
                "src/factory.ts",
                &crate::client_semantics::LibrarySemantics::default()
            )
        );
    }

    /// carrick#1790: a client a getter builds once into a module-scope `let`
    /// is read by the message roles ([`crate::request_summary::library_sites`]);
    /// an HTTP reading reads nothing through it, whatever the semantics
    /// verify, held in a function's `const` or called on directly.
    #[test]
    fn a_client_a_getter_builds_once_states_no_http_row() {
        let (dir, discovery) = discover_sources(&[(
            "src/lazy.ts",
            "import http from \"@fixture/http\";\n\
             let api: any = null;\n\
             export function getApi() {\n\
             \x20 if (!api) {\n\
             \x20   api = http.create({ baseURL: \"/v1\" });\n\
             \x20 }\n\
             \x20 return api;\n\
             }\n\
             export function load() { return getApi().get(\"/users\"); }\n\
             export function local() { const inner = getApi(); return inner.get(\"/teams\"); }\n",
        )]);
        let verified = library_rows_of(&dir, &discovery, "src/lazy.ts", &verified_sample());
        assert!(
            verified.iter().all(|row| row.library_semantics.is_empty()),
            "{verified:#?}"
        );
        assert_eq!(
            verified,
            library_rows_of(
                &dir,
                &discovery,
                "src/lazy.ts",
                &crate::client_semantics::LibrarySemantics::default()
            )
        );
    }

    /// carrick#1564: a wrapper handed the path is stated at the call that
    /// fills it, and a call through a declaration in another module reaches
    /// the request line, both joined to the instance's base.
    #[test]
    fn a_library_request_composes_across_modules_like_any_other() {
        let (dir, discovery) = discover_sources(&[
            ("src/api.ts", HTTP_CLIENT),
            (
                "src/use.ts",
                "import { listUsers, send } from \"./api\";\n\nexport async function go() {\n  await listUsers();\n  return send(\"/teams\");\n}\n",
            ),
        ]);
        let rows = library_rows_of(&dir, &discovery, "src/use.ts", &verified_sample());
        assert_eq!(rows.len(), 2, "{rows:#?}");
        let through = rows.iter().find(|row| row.line == 4).expect("line 4");
        assert_eq!(through.target, "/api/v1/users");
        assert!(
            through
                .reaches_request
                .as_deref()
                .is_some_and(|site| site.ends_with("src/api.ts:7")),
            "{through:#?}"
        );
        let filled = rows.iter().find(|row| row.line == 5).expect("line 5");
        assert_eq!(
            (filled.method.as_str(), filled.target.as_str()),
            ("GET", "/api/v1/teams")
        );
        assert_eq!(filled.reaches_request, None, "this line states the request");
        assert!(!filled.library_semantics.is_empty());
    }

    /// carrick#1564: the prefix-style client: a factory keyed on another
    /// option, a path written without its leading slash, and the body under
    /// the options' own key.
    #[test]
    fn a_prefix_style_instance_joins_with_one_slash_and_reads_its_body_key() {
        let (dir, discovery) = discover_sources(&[(
            "src/jobs.ts",
            "import client from \"fixture-prefix-http\";\n\nexport class Jobs {\n  private http = client.create({ prefixUrl: \"/svc/\" });\n\n  list() {\n    return this.http.get(\"jobs\");\n  }\n\n  run() {\n    return this.http.post(\"/jobs/run\", { json: { op: \"start\" }, retry: 2 });\n  }\n}\n",
        )]);
        let rows = library_rows_of(&dir, &discovery, "src/jobs.ts", &verified_sample());
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            (
                rows[0].line,
                rows[0].method.as_str(),
                rows[0].target.as_str()
            ),
            (7, "GET", "/svc/jobs")
        );
        assert_eq!(
            (
                rows[1].line,
                rows[1].method.as_str(),
                rows[1].target.as_str()
            ),
            (11, "POST", "/svc/jobs/run")
        );
        assert_eq!(
            rows[1].body_literals.get("op").map(String::as_str),
            Some("start")
        );
    }

    /// carrick#1564: what is not the client is never read as one. Without
    /// semantics the client's own sites state nothing (the verb rule), a
    /// `Map` built in place or bound to the client's name reaches no claim,
    /// a field written twice holds no instance, and a factory handed
    /// anything but one object literal builds nothing.
    #[test]
    fn nothing_but_the_named_client_reaches_a_claim() {
        let (dir, discovery) = discover_sources(&[
            ("src/api.ts", HTTP_CLIENT),
            (
                "src/negatives.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const options = { baseURL: \"/opts\" };\n\
                 const indirect = http.create(options);\n\
                 \n\
                 export function cached() {\n\
                 \x20 return new Map<string, string>().get(\"/r\");\n\
                 }\n\
                 \n\
                 export function viaOptions() {\n\
                 \x20 return indirect.get(\"/indirect\");\n\
                 }\n\
                 \n\
                 export class Moving {\n\
                 \x20 private api = http.create({ baseURL: \"/one\" });\n\
                 \x20 reset() {\n\
                 \x20   this.api = http.create({ baseURL: \"/two\" });\n\
                 \x20 }\n\
                 \x20 load() {\n\
                 \x20   return this.api.get(\"/moving\");\n\
                 \x20 }\n\
                 }\n",
            ),
            (
                "src/shadow.ts",
                "import http from \"@fixture/http\";\n\nexport function shadowed() {\n  const http = new Map<string, string>();\n  return http.get(\"/shadow\");\n}\n\nexport function looped(keys: string[]) {\n  for (const http of [new Map<string, string>()]) {\n    http.get(\"/loop\");\n  }\n}\n",
            ),
            // A block inside the function declares the instance's name again:
            // nothing tracks which one a use means, so neither is read.
            (
                "src/block.ts",
                "import http from \"@fixture/http\";\n\nexport function block() {\n  const api = http.create({ baseURL: \"/outer\" });\n  {\n    const api = new Map<string, string>();\n    api.get(\"/inner-map\");\n  }\n  return api.get(\"/outer-ok\");\n}\n",
            ),
        ]);
        // Without verified semantics a verb call states no row at its own
        // site; the config-object call at line 23 is a request by its own
        // shape, as it always was, and none is read through a claim.
        let unverified = crate::client_semantics::LibrarySemantics::default();
        let plain = library_rows_of(&dir, &discovery, "src/api.ts", &unverified);
        assert_eq!(
            plain.iter().map(|row| row.line).collect::<Vec<_>>(),
            vec![23],
            "{plain:#?}"
        );
        assert!(plain.iter().all(|row| row.library_semantics.is_empty()));
        for file in ["src/negatives.ts", "src/shadow.ts", "src/block.ts"] {
            let rows = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(rows.is_empty(), "{file}: {rows:#?}");
        }
    }

    /// carrick#1564 review, findings 1 and 3: wherever the source may set the
    /// base itself, nothing is read through the client. A base key written
    /// through the client after the factory ran, options open to a spread,
    /// and an options or config object that names a base key or is open, all
    /// leave the call as it reads without the semantics. The data a `(path,
    /// body)` verb sends is not options, so a spread there changes nothing.
    #[test]
    fn a_base_the_source_may_set_elsewhere_reads_nothing_through_the_client() {
        let (dir, discovery) = discover_sources(&[(
            "src/overrides.ts",
            "import http from \"@fixture/http\";\n\
             \n\
             declare const overrides: { baseURL?: string };\n\
             const api = http.create({ baseURL: \"/v1\" });\n\
             const mutated = http.create({ baseURL: \"/v1\" });\n\
             (mutated as any).defaults.baseURL = \"/v2\";\n\
             const spread = http.create({ baseURL: \"/v1\", ...overrides });\n\
             \n\
             export function a() { return mutated.get(\"/mutated\"); }\n\
             export function b() { return api.get(\"/users\", { baseURL: \"/override\" } as any); }\n\
             export function c() { return http.get(\"/plain\", { baseURL: \"/elsewhere\" } as any); }\n\
             export function d() { return spread.get(\"/spread\"); }\n\
             export function e(cfg: object) { return http.request({ url: \"/cfg\", method: \"post\", ...cfg }); }\n\
             export function f(d: unknown) { return api.post(\"/body\", d, { baseURL: \"/over\" } as any); }\n\
             export function g(order: object) { return api.post(\"/orders\", { ...order, status: \"new\" }); }\n",
        )]);
        let rows = library_rows_of(&dir, &discovery, "src/overrides.ts", &verified_sample());
        let stated: Vec<(u32, &str, &str)> = rows
            .iter()
            .map(|row| (row.line, row.method.as_str(), row.target.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(15, "POST", "/v1/orders")],
            "only the call whose body alone is spread reads through the client: {rows:#?}"
        );
        assert_eq!(
            rows[0].body_literals.get("status").map(String::as_str),
            Some("new"),
            "a key written after the spread is the body's own"
        );
    }

    /// carrick#1564 re-review, R1: a base key the factory options write after
    /// every entry that could overwrite it is read, however open the options
    /// are; one a later spread, a getter, a computed key or a disagreeing
    /// conditional spread may overwrite is not. `cond && {…}` may spread
    /// nothing, so the key it carries is not the only value the base can hold.
    #[test]
    fn a_base_written_after_everything_that_could_overwrite_it_is_read() {
        let (dir, discovery) = discover_sources(&[(
            "src/options.ts",
            "import http from \"@fixture/http\";\n\
             \n\
             declare const defaults: { timeout?: number };\n\
             declare const a: object;\n\
             declare const b: object;\n\
             declare const prod: boolean;\n\
             const KEY = \"baseURL\";\n\
             const after = http.create({ ...defaults, baseURL: \"/after\" });\n\
             const between = http.create({ ...a, baseURL: \"/between\", ...b });\n\
             const cond = http.create({ baseURL: \"/dev\", ...(prod ? { baseURL: \"/prod\" } : {}) });\n\
             const same = http.create({ baseURL: \"/x\", ...(prod ? { timeout: 1 } : { timeout: 2 }) });\n\
             const agree = http.create({ ...(prod ? { baseURL: \"/same\" } : { baseURL: \"/same\" }) });\n\
             const andSpread = http.create({ baseURL: \"/and\", ...(prod && { baseURL: \"/and-prod\" }) });\n\
             const getter = http.create({ baseURL: \"/g1\", get baseURL() { return \"/g2\"; } } as any);\n\
             const computed = http.create({ baseURL: \"/c1\", [KEY]: \"/c2\" });\n\
             const strKey = http.create({ \"baseURL\": \"/s1\" });\n\
             const methodProp = http.create({ baseURL: \"/m1\", timeout() { return 1; } } as any);\n\
             \n\
             export function n1() { return after.get(\"/stated\"); }\n\
             export function n2() { return between.get(\"/between\"); }\n\
             export function n3a() { return cond.get(\"/cond\"); }\n\
             export function n3b() { return same.get(\"/keep\"); }\n\
             export function n3c() { return agree.get(\"/agree\"); }\n\
             export function n3d() { return andSpread.get(\"/and\"); }\n\
             export function n4a() { return getter.get(\"/getter\"); }\n\
             export function n4b() { return computed.get(\"/computed\"); }\n\
             export function n4c() { return strKey.get(\"/string-key\"); }\n\
             export function n4d() { return methodProp.get(\"/method-prop\"); }\n",
        )]);
        let rows = library_rows_of(&dir, &discovery, "src/options.ts", &verified_sample());
        let stated: Vec<(u32, &str, &str)> = rows
            .iter()
            .map(|row| (row.line, row.method.as_str(), row.target.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![
                (19, "GET", "/after/stated"),
                (22, "GET", "/x/keep"),
                (23, "GET", "/same/agree"),
                (27, "GET", "/s1/string-key"),
                (28, "GET", "/m1/method-prop"),
            ],
            "{rows:#?}"
        );
    }

    /// carrick#1564 re-review, R6: a client binding the file uses for
    /// anything but calls through it, or exporting it, holds no client: an
    /// alias, an argument, a member read that is not called, or a write
    /// through a function-local instance may each change its base. A write
    /// through the export before the factory runs reaches the instance too.
    /// In a static arrow property `this` is the class, which holds no
    /// instance field.
    #[test]
    fn a_client_used_other_than_to_call_through_it_holds_no_client() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/uses.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const api = http.create({ baseURL: \"/v1\" });\n\
                 const defaults = (api as any).defaults;\n\
                 defaults.baseURL = \"/v2\";\n\
                 \n\
                 const api2 = http.create({ baseURL: \"/w1\" });\n\
                 Object.assign((api2 as any).defaults, { baseURL: \"/w2\" });\n\
                 \n\
                 const passed = http.create({ baseURL: \"/p1\" });\n\
                 configure(passed);\n\
                 \n\
                 const kept = http.create({ baseURL: \"/kept\" });\n\
                 export default kept;\n\
                 \n\
                 declare function configure(client: unknown): void;\n\
                 \n\
                 export function n8a() { return api.get(\"/alias-write\"); }\n\
                 export function n8b() { return api2.get(\"/assign-write\"); }\n\
                 export function passedOn() { return passed.get(\"/passed\"); }\n\
                 export function exported() { return kept.get(\"/exported\"); }\n\
                 \n\
                 export function local() {\n\
                 \x20 const scoped = http.create({ baseURL: \"/local\" });\n\
                 \x20 scoped.defaults.baseURL = \"/elsewhere\";\n\
                 \x20 return scoped.get(\"/local-write\");\n\
                 }\n\
                 \n\
                 export class Holder {\n\
                 \x20 private api = http.create({ baseURL: \"/inst\" });\n\
                 \x20 static load = () => this.api.get(\"/static-arrow\");\n\
                 \x20 run() { return this.api.get(\"/ran\"); }\n\
                 }\n\
                 \n\
                 export class Retarget {\n\
                 \x20 private svc = http.create({ baseURL: \"/dw\" });\n\
                 \x20 retarget() { this.svc.defaults.baseURL = \"/elsewhere\"; }\n\
                 \x20 list() { return this.svc.get(\"/dw-list\"); }\n\
                 }\n\
                 \n\
                 const short = http.create({ baseURL: \"/short\" });\n\
                 register({ short });\n\
                 declare function register(clients: object): void;\n\
                 export function viaShort() { return short.get(\"/shorthand\"); }\n\
                 \n\
                 const cjs = http.create({ baseURL: \"/cjs\" });\n\
                 module.exports.cjs = cjs;\n\
                 export function viaCjs() { return cjs.get(\"/commonjs\"); }\n\
                 \n\
                 const opt = http.create({ baseURL: \"/opt\" });\n\
                 export function maybe() { return opt?.get(\"/optional\"); }\n\
                 export function plainCall() { return opt.get(\"/plain-call\"); }\n",
            ),
            (
                "src/export-defaults.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 http.defaults.baseURL = \"/configured\";\n\
                 const built = http.create({ timeout: 5 });\n\
                 \n\
                 export function plain() { return http.get(\"/plain\"); }\n\
                 export function fromBuilt() { return built.get(\"/built\"); }\n",
            ),
        ]);
        let rows = library_rows_of(&dir, &discovery, "src/uses.ts", &verified_sample());
        let stated: Vec<(u32, &str)> = rows
            .iter()
            .map(|row| (row.line, row.target.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![
                (21, "/kept/exported"),
                (32, "/inst/ran"),
                (48, "/cjs/commonjs"),
                (52, "/opt/plain-call"),
            ],
            "{rows:#?}"
        );
        let rows = library_rows_of(
            &dir,
            &discovery,
            "src/export-defaults.ts",
            &verified_sample(),
        );
        assert!(rows.is_empty(), "{rows:#?}");
    }

    /// carrick#1564, third review: a call through an instance to a member
    /// outside its verified surface may change its base, so the instance
    /// holds no client; one that only calls verified verbs still does.
    #[test]
    fn a_call_outside_the_instance_surface_holds_no_client() {
        let (dir, discovery) = discover_sources(&[(
            "src/surface.ts",
            "import http from \"@fixture/http\";\n\
             \n\
             const setter = http.create({ baseURL: \"/r5\" });\n\
             (setter as any).setBaseURL(\"/elsewhere\");\n\
             const keyed = http.create({ baseURL: \"/keyed\" });\n\
             declare const name: string;\n\
             (keyed as any)[name]();\n\
             const verbs = http.create({ baseURL: \"/verbs\" });\n\
             \n\
             export function r5() { return setter.get(\"/setter\"); }\n\
             export function viaKey() { return keyed.get(\"/keyed-call\"); }\n\
             export function other() { return verbs.post(\"/other\", {}); }\n\
             export function viaVerbs() { return verbs.get(\"/verbs-only\"); }\n",
        )]);
        let rows = library_rows_of(&dir, &discovery, "src/surface.ts", &verified_sample());
        let stated: Vec<(u32, &str)> = rows
            .iter()
            .map(|row| (row.line, row.target.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(12, "/verbs/other"), (13, "/verbs/verbs-only")],
            "{rows:#?}"
        );
    }

    /// carrick#1564, third review: an object constant the file writes
    /// through holds keys its literal does not state, read directly as well
    /// as spread. A clean constant still states its keys.
    #[test]
    fn a_constant_the_file_writes_through_states_no_key_read_directly() {
        let (dir, discovery) = discover_sources(&[(
            "src/direct.ts",
            "const U1 = \"/api/p1\";\n\
             const U4 = \"/api/p4\";\n\
             const U6 = \"/api/p6\";\n\
             const U7 = \"/api/p7\";\n\
             const WRITTEN = { method: \"POST\" };\n\
             WRITTEN.method = \"PUT\";\n\
             const CLEAN = { method: \"PATCH\" };\n\
             const NOMETHOD = { headers: { a: \"b\" } };\n\
             const READ = { method: \"POST\" };\n\
             delete (READ as any).method;\n\
             \n\
             export function p1() { return fetch(U1, WRITTEN); }\n\
             export function p4() { return fetch(U4, CLEAN); }\n\
             export function p6() { return fetch(U6, NOMETHOD); }\n\
             export function p7() { return fetch(U7, { method: READ.method }); }\n",
        )]);
        let rows = summary_rows_of(&dir, &discovery, "src/direct.ts");
        let stated: Vec<(u32, &str, &str)> = rows
            .iter()
            .map(|row| (row.line, row.method.as_str(), row.target.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(13, "PATCH", "/api/p4"), (14, "GET", "/api/p6")],
            "{rows:#?}"
        );
    }

    /// (line, method, target) of every library row the summaries state for
    /// `file` with the contract sample verified.
    fn library_stated(
        dir: &tempfile::TempDir,
        discovery: &FileDiscovery,
        file: &str,
    ) -> Vec<(u32, String, String)> {
        library_rows_of(dir, discovery, file, &verified_sample())
            .into_iter()
            .map(|row| {
                assert!(
                    !row.library_semantics.is_empty(),
                    "{file}: only library rows are expected here: {row:#?}"
                );
                (row.line, row.method, row.target)
            })
            .collect()
    }

    fn stated(rows: &[(u32, &str, &str)]) -> Vec<(u32, String, String)> {
        rows.iter()
            .map(|(line, method, target)| (*line, method.to_string(), target.to_string()))
            .collect()
    }

    /// The module the next tests import the instance from.
    const SHARED_API: &str = "import http from \"@fixture/http\";\n\
        \n\
        export const api = http.create({ baseURL: \"/api/v1\" });\n\
        \n\
        export function health() { return api.get(\"/health\"); }\n";

    /// carrick#1568: an instance one module builds and exports is the client
    /// in every module that imports it, read with its declaring module's
    /// options and claims.
    #[test]
    fn an_instance_imported_from_another_module_reads_through_its_declaration() {
        let (dir, discovery) = discover_service(&[
            ("src/lib/api.ts", SHARED_API),
            (
                "src/users.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export function listUsers() { return api.get(\"/users\"); }\n",
            ),
            (
                "src/orders.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export function createOrder() {\n\
                 \x20 return api.post(\"/orders\", { action: \"create\" });\n\
                 }\n",
            ),
        ]);
        let users = library_rows_of(&dir, &discovery, "src/users.ts", &verified_sample());
        assert_eq!(users.len(), 1, "{users:#?}");
        assert_eq!(
            (
                users[0].line,
                users[0].method.as_str(),
                users[0].target.as_str()
            ),
            (3, "GET", "/api/v1/users")
        );
        assert_eq!(
            users[0].library_semantics,
            vec![
                "@fixture/http@1:default:factory:create",
                "@fixture/http@1:default:verb:get",
            ]
        );
        assert!(!users[0].own_site, "claims the site like any library row");
        let orders = library_rows_of(&dir, &discovery, "src/orders.ts", &verified_sample());
        assert_eq!(orders.len(), 1, "{orders:#?}");
        assert_eq!(
            (
                orders[0].line,
                orders[0].method.as_str(),
                orders[0].target.as_str()
            ),
            (4, "POST", "/api/v1/orders")
        );
        assert_eq!(
            orders[0].body_literals.get("action").map(String::as_str),
            Some("create")
        );
        assert_eq!(
            library_stated(&dir, &discovery, "src/lib/api.ts"),
            stated(&[(5, "GET", "/api/v1/health")]),
            "the declaring module still reads its own calls"
        );
    }

    /// carrick#1568: the resolver's own hops carry the instance through a
    /// barrel (`export { default as svc } from`, `export *`), a renaming
    /// import, a default import of an anonymous `export default <factory>`,
    /// an `export { client as name }`, and a module that re-exports a binding
    /// it imports.
    #[test]
    fn an_instance_reached_through_a_barrel_a_rename_or_a_default_reads_through_it() {
        let (dir, discovery) = discover_service(&[
            ("src/lib/api.ts", SHARED_API),
            (
                "src/clients/svc.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export default http.create({ baseURL: \"/svc\" });\n",
            ),
            (
                "src/clients/named.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const client = http.create({ baseURL: \"/named\" });\n\
                 \n\
                 export { client as namedClient };\n",
            ),
            (
                "src/clients/index.ts",
                "export { default as svc } from \"./svc\";\n\
                 export * from \"./named\";\n",
            ),
            (
                "src/reexport.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export { api };\n",
            ),
            (
                "src/use.ts",
                "import { svc, namedClient as nc } from \"./clients\";\n\
                 import direct from \"./clients/svc\";\n\
                 import { api as shared } from \"./reexport\";\n\
                 \n\
                 export const a = () => svc.get(\"/a\");\n\
                 export const b = () => nc.get(\"/b\");\n\
                 export const c = () => direct.get(\"/c\");\n\
                 export const d = () => shared.get(\"/d\");\n",
            ),
        ]);
        assert_eq!(
            library_stated(&dir, &discovery, "src/use.ts"),
            stated(&[
                (5, "GET", "/svc/a"),
                (6, "GET", "/named/b"),
                (7, "GET", "/svc/c"),
                (8, "GET", "/api/v1/d"),
            ])
        );
    }

    /// carrick#1568, must not resolve: wherever any module may change the
    /// instance, or the export may not hold it, no module reads through it.
    /// A write after the export, a write in a module that only imports it, a
    /// call outside its verified surface or a hand-off in another importer,
    /// `export let`, an export its own module contests, and one published
    /// through `exports`.
    #[test]
    fn an_instance_any_module_may_change_reads_nothing_in_any_module() {
        let importer = |from: &str, path: &str| {
            format!(
                "import {{ api }} from \"{from}\";\n\
                 \n\
                 export function call() {{ return api.get(\"{path}\"); }}\n"
            )
        };
        let (dir, discovery) = discover_service(&[
            // Written through after the export, in its own module.
            (
                "src/written/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export const api = http.create({ baseURL: \"/w\" });\n\
                 (api as any).defaults.baseURL = \"/v2\";\n",
            ),
            ("src/written/use.ts", &importer("./api", "/written")),
            // Written through in a module that never calls it: the
            // declaring module's own call and the other importer's go too.
            (
                "src/elsewhere/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export const api = http.create({ baseURL: \"/e\" });\n\
                 \n\
                 export function own() { return api.get(\"/own\"); }\n",
            ),
            ("src/elsewhere/use.ts", &importer("./api", "/elsewhere")),
            (
                "src/elsewhere/configure.ts",
                "import { api } from \"./api\";\n\
                 \n\
                 (api as any).defaults.baseURL = \"/v2\";\n",
            ),
            // A member outside the verified surface called by an importer.
            (
                "src/setter/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export const api = http.create({ baseURL: \"/s\" });\n",
            ),
            ("src/setter/use.ts", &importer("./api", "/setter")),
            (
                "src/setter/retarget.ts",
                "import { api } from \"./api\";\n\
                 \n\
                 export function retarget() { (api as any).setBaseURL(\"/v2\"); }\n",
            ),
            // Handed to a helper by an importer.
            (
                "src/passed/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export const api = http.create({ baseURL: \"/p\" });\n",
            ),
            ("src/passed/use.ts", &importer("./api", "/passed")),
            (
                "src/passed/register.ts",
                "import { api } from \"./api\";\n\
                 \n\
                 declare function register(client: unknown): void;\n\
                 register(api);\n",
            ),
            // `export let`: the binding may hold something else by the time
            // an importer calls through it.
            (
                "src/mutable/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 export let api = http.create({ baseURL: \"/m\" });\n",
            ),
            ("src/mutable/use.ts", &importer("./api", "/mutable")),
            // Contested in its defining module: the export's nested member.
            (
                "src/contested/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 (http as any).interceptors.request.use((c: unknown) => c);\n\
                 export const api = http.create({ baseURL: \"/c\" });\n",
            ),
            ("src/contested/use.ts", &importer("./api", "/contested")),
            // Published through `exports`, which a write anywhere in the
            // module may replace.
            (
                "src/commonjs/api.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const api = http.create({ baseURL: \"/cjs\" });\n\
                 module.exports.api = api;\n\
                 \n\
                 export function own() { return api.get(\"/own\"); }\n",
            ),
            ("src/commonjs/use.ts", &importer("./api", "/commonjs")),
        ]);
        for file in [
            "src/written/use.ts",
            "src/elsewhere/api.ts",
            "src/elsewhere/use.ts",
            "src/setter/use.ts",
            "src/passed/use.ts",
            "src/mutable/use.ts",
            "src/contested/use.ts",
            "src/commonjs/use.ts",
        ] {
            let rows = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(rows.is_empty(), "{file}: {rows:#?}");
        }
        assert_eq!(
            library_stated(&dir, &discovery, "src/commonjs/api.ts"),
            stated(&[(6, "GET", "/cjs/own")]),
            "a module publishing through `exports` still reads its own calls"
        );
    }

    /// carrick#1568: a file that declares the imported name again anywhere
    /// below module scope reads no call through it, since nothing says which
    /// binding a use means; the other importers are unaffected.
    #[test]
    fn an_imported_name_declared_again_reads_nothing_in_that_file() {
        let (dir, discovery) = discover_service(&[
            ("src/lib/api.ts", SHARED_API),
            (
                "src/shadowed.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 interface Getter { get(path: string): unknown }\n\
                 export function viaParam(api: Getter) { return api.get(\"/shadowed\"); }\n\
                 export function viaImport() { return api.get(\"/import\"); }\n",
            ),
            (
                "src/users.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export function listUsers() { return api.get(\"/users\"); }\n",
            ),
        ]);
        let rows = library_rows_of(&dir, &discovery, "src/shadowed.ts", &verified_sample());
        assert!(rows.is_empty(), "{rows:#?}");
        assert_eq!(
            library_stated(&dir, &discovery, "src/users.ts"),
            stated(&[(3, "GET", "/api/v1/users")])
        );
    }

    /// carrick#1568, known gaps: a client reached through a namespace import
    /// (`lib.api.get()`) is read through nothing, and reaching it that way
    /// reads a member of the namespace that is not called, which may change
    /// it: no module reads through it. Nor does a module outside the
    /// service's own files, whose other importers the scan cannot see.
    #[test]
    fn a_client_reached_through_a_namespace_reads_nothing_anywhere() {
        let (dir, discovery) = discover_service(&[
            ("src/lib/api.ts", SHARED_API),
            (
                "src/users.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export function listUsers() { return api.get(\"/users\"); }\n",
            ),
            (
                "src/namespace.ts",
                "import * as lib from \"./lib/api\";\n\
                 \n\
                 export function viaNamespace() { return lib.api.get(\"/ns\"); }\n",
            ),
        ]);
        for file in ["src/namespace.ts", "src/users.ts", "src/lib/api.ts"] {
            let rows = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(rows.is_empty(), "{file}: {rows:#?}");
        }

        // A namespace only called through (`lib.health()`) changes nothing.
        let (dir, discovery) = discover_service(&[
            ("src/lib/api.ts", SHARED_API),
            (
                "src/users.ts",
                "import { api } from \"./lib/api\";\n\
                 \n\
                 export function listUsers() { return api.get(\"/users\"); }\n",
            ),
            (
                "src/namespace.ts",
                "import * as lib from \"./lib/api\";\n\
                 \n\
                 export function ping() { return lib.health(); }\n",
            ),
        ]);
        assert_eq!(
            library_stated(&dir, &discovery, "src/users.ts"),
            stated(&[(3, "GET", "/api/v1/users")])
        );
    }

    /// carrick#1568: an `instanceof`, `typeof` or comparison read of the
    /// export, or of an instance, keeps nothing of it and contests nothing
    /// (the #1571 review's `r4b-instanceof.ts`). A nested member call, a read
    /// handed on and a write still do.
    #[test]
    fn a_compared_read_of_a_client_contests_nothing() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/compared.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const api = http.create({ baseURL: \"/r4b\" });\n\
                 \n\
                 export function isHttpError(e: unknown): boolean { return e instanceof (http as any).HttpError; }\n\
                 export function kind(): string { return typeof http; }\n\
                 export function same(other: unknown): boolean { return api === other || (api as any).defaults !== other; }\n\
                 export function r4b(): unknown { return api.get(\"/instanceof\"); }\n",
            ),
            (
                "src/nested.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const api = http.create({ baseURL: \"/nested\" });\n\
                 (api as any).interceptors.request.use((c: unknown) => c);\n\
                 \n\
                 export function nested(): unknown { return api.get(\"/nested\"); }\n",
            ),
            (
                "src/handed.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 const api = http.create({ baseURL: \"/handed\" });\n\
                 console.log(typeof api, (api as any).defaults);\n\
                 \n\
                 export function handed(): unknown { return api.get(\"/handed\"); }\n",
            ),
        ]);
        assert_eq!(
            library_stated(&dir, &discovery, "src/compared.ts"),
            stated(&[(8, "GET", "/r4b/instanceof")])
        );
        for file in ["src/nested.ts", "src/handed.ts"] {
            let rows = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(rows.is_empty(), "{file}: {rows:#?}");
        }
    }

    /// A module that builds an instance for others to import, and one that
    /// imports it and calls it, under `dir`.
    fn api_and_reader(dir: &str, base: &str) -> [(String, String); 2] {
        [
            (
                format!("src/{dir}/api.ts"),
                format!(
                    "import http from \"@fixture/http\";\n\
                     export const api = http.create({{ baseURL: \"{base}\" }});\n\
                     export function own() {{ return api.get(\"/own\"); }}\n"
                ),
            ),
            (
                format!("src/{dir}/reader.ts"),
                "import { api } from \"./api\";\n\
                 export function r() { return api.get(\"/read\"); }\n"
                    .to_string(),
            ),
        ]
    }

    /// The manifest a service using the contract sample's packages declares
    /// them in. Without it they are undeclared packages, which name nothing
    /// the scan can find and turn imported reading off (carrick#1568).
    const SAMPLE_MANIFEST: &str = "{ \"name\": \"service\", \"dependencies\": { \"@fixture/http\": \"^1.4.0\", \"fixture-prefix-http\": \"^2.1.0\" } }\n";

    /// A tsconfig mapping `@/*` onto `src/`.
    const PATHS_TSCONFIG: &str =
        "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": [\"./src/*\"] } } }\n";

    /// Discovery over `files`, owned strings, in a service whose manifest
    /// declares the contract sample's packages unless `files` bring one.
    fn discover_owned(files: &[(String, String)]) -> (tempfile::TempDir, FileDiscovery) {
        let mut borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, source)| (name.as_str(), source.as_str()))
            .collect();
        if !borrowed.iter().any(|(name, _)| *name == "package.json") {
            borrowed.push(("package.json", SAMPLE_MANIFEST));
        }
        discover_sources(&borrowed)
    }

    /// [`discover_owned`] over borrowed sources.
    fn discover_service(files: &[(&str, &str)]) -> (tempfile::TempDir, FileDiscovery) {
        let owned: Vec<(String, String)> = files
            .iter()
            .map(|(name, source)| (name.to_string(), source.to_string()))
            .collect();
        discover_owned(&owned)
    }

    /// carrick#1568 fix round 1, F2: a module loaded other than through an
    /// import binding may do anything to what it publishes, so every client
    /// it publishes reads nothing, in its own module and in every importer:
    /// `import()` inside a function or chained, `require` inline, inside a
    /// function, bound by `let`, `var` or `export const`, a no-hole template
    /// specifier, and a `require` a factory made (`req("./api")`). A
    /// specifier the source computes names no module and contests nothing,
    /// a stated limit.
    #[test]
    fn a_module_loaded_other_than_by_an_import_binding_contests_what_it_publishes() {
        let loaders: [(&str, &str); 8] = [
            (
                "g01",
                "export async function boot() {\n  const m = await import(\"./api\");\n  (m.api as any).defaults.baseURL = \"https://elsewhere.example\";\n}\n",
            ),
            (
                "g02",
                "import(\"./api\").then((m) => { (m.api as any).defaults.baseURL = \"https://elsewhere.example\"; });\nexport {};\n",
            ),
            (
                "g03",
                "declare const require: (s: string) => any;\nrequire(\"./api\").api.defaults.baseURL = \"https://elsewhere.example\";\nexport {};\n",
            ),
            (
                "g04",
                "declare const require: (s: string) => any;\nexport function setup() {\n  const { api } = require(\"./api\");\n  api.defaults.baseURL = \"https://elsewhere.example\";\n}\n",
            ),
            (
                "g35",
                "let { api } = require(\"./api\");\napi.defaults.baseURL = \"https://elsewhere.example\";\n",
            ),
            (
                "g36",
                "var m = require(\"./api\");\nm.api.defaults.baseURL = \"https://elsewhere.example\";\n",
            ),
            (
                "g37",
                "declare const require: (s: string) => any;\nexport const { api } = require(\"./api\");\napi.defaults.baseURL = \"https://elsewhere.example\";\n",
            ),
            (
                "g41",
                "export async function boot() {\n  const { api } = await import(`./api`);\n  api.defaults.baseURL = \"https://elsewhere.example\";\n}\n",
            ),
        ];
        let mut files: Vec<(String, String)> = Vec::new();
        for (dir, boot) in loaders {
            files.extend(api_and_reader(dir, &format!("/{dir}")));
            let extension = if dir == "g35" || dir == "g36" {
                "js"
            } else {
                "ts"
            };
            files.push((format!("src/{dir}/boot.{extension}"), boot.to_string()));
        }
        files.extend(api_and_reader("g42", "/g42"));
        files.push((
            "src/g42/boot.ts".to_string(),
            "import { createRequire } from \"module\";\nconst req = createRequire(import.meta.url);\nreq(\"./api\").api.defaults.baseURL = \"https://elsewhere.example\";\n".to_string(),
        ));
        // Bound by `let` and only called through: its uses are followed like
        // an import's, so it takes nothing away.
        files.extend(api_and_reader("letcalls", "/letcalls"));
        files.push((
            "src/letcalls/boot.js".to_string(),
            "let { api } = require(\"./api\");\napi.get(\"/boot\");\n".to_string(),
        ));
        files.extend(api_and_reader("computed", "/computed"));
        files.push((
            "src/computed/boot.ts".to_string(),
            "export async function boot(name: string) {\n  const m = await import(name);\n  m.api.defaults.baseURL = \"https://elsewhere.example\";\n}\n".to_string(),
        ));
        let (dir, discovery) = discover_owned(&files);
        for case in [
            "g01", "g02", "g03", "g04", "g35", "g36", "g37", "g41", "g42",
        ] {
            for file in ["api", "reader"] {
                let path = format!("src/{case}/{file}.ts");
                let rows = library_rows_of(&dir, &discovery, &path, &verified_sample());
                assert!(rows.is_empty(), "{path}: {rows:#?}");
            }
        }
        assert_eq!(
            library_stated(&dir, &discovery, "src/computed/reader.ts"),
            stated(&[(2, "GET", "/computed/read")]),
            "a computed specifier names no module (a stated limit)"
        );
        assert_eq!(
            library_stated(&dir, &discovery, "src/letcalls/reader.ts"),
            stated(&[(2, "GET", "/letcalls/read")]),
            "a require bound by `let` and only called through changes nothing"
        );
    }

    /// carrick#1568 fix round 1, F2: where the resolver stops at a cap
    /// before it can say what a use names (a re-export chain past its hop
    /// limit, a module re-exporting a binding it imports past this pass's
    /// own limit, an `export *` fan-out past its visit limit), the use may be
    /// of any instance, so no import in the service reads through one. The
    /// declaring modules still read their own calls, as they did before any
    /// import was followed.
    #[test]
    fn a_use_the_resolver_cannot_follow_turns_imported_reading_off() {
        let chain = |dir: &str, hop: &dyn Fn(usize) -> String| -> Vec<(String, String)> {
            let mut files: Vec<(String, String)> = api_and_reader(dir, &format!("/{dir}")).into();
            for n in 1..=10 {
                files.push((format!("src/{dir}/h{n}.ts"), hop(n)));
            }
            files.push((
                format!("src/{dir}/boot.ts"),
                "import { api } from \"./h10\";\n(api as any).defaults.baseURL = \"https://elsewhere.example\";\n".to_string(),
            ));
            files
        };
        let from = |n: usize| {
            if n == 1 {
                "./api".to_string()
            } else {
                format!("./h{}", n - 1)
            }
        };
        let forwarded = chain("g10", &|n| {
            format!("export {{ api }} from \"{}\";\n", from(n))
        });
        let reexported = chain("g11", &|n| {
            format!(
                "import {{ api }} from \"{}\";\nexport {{ api }};\n",
                from(n)
            )
        });
        // A default import takes the value walk alone, with no namespace
        // probe ahead of it.
        let mut defaulted: Vec<(String, String)> = api_and_reader("g10d", "/g10d").into();
        for n in 1..=10 {
            let hop = if n == 1 {
                "export { api as default } from \"./api\";\n".to_string()
            } else {
                format!("export {{ default }} from \"./h{}\";\n", n - 1)
            };
            defaulted.push((format!("src/g10d/h{n}.ts"), hop));
        }
        defaulted.push((
            "src/g10d/boot.ts".to_string(),
            "import api from \"./h10\";\n(api as any).defaults.baseURL = \"https://elsewhere.example\";\n"
                .to_string(),
        ));
        let mut fanned: Vec<(String, String)> = api_and_reader("g13", "/g13").into();
        let mut index = String::new();
        for n in 0..70 {
            fanned.push((
                format!("src/g13/f{n}.ts"),
                format!("export const v{n} = {n};\n"),
            ));
            index.push_str(&format!("export * from \"./f{n}\";\n"));
        }
        index.push_str("export * from \"./api\";\n");
        fanned.push(("src/g13/index.ts".to_string(), index));
        let with_boot = |boot: &str| -> Vec<(String, String)> {
            let mut files = fanned.clone();
            files.push(("src/g13/boot.ts".to_string(), boot.to_string()));
            files
        };
        let named = with_boot(
            "import { api } from \"./index\";\n(api as any).defaults.baseURL = \"https://elsewhere.example\";\n",
        );
        let namespace = with_boot(
            "import * as all from \"./index\";\n(all as any).api.defaults.baseURL = \"https://elsewhere.example\";\n",
        );
        let loaded = with_boot(
            "import(\"./index\").then((m: any) => { m.api.defaults.baseURL = \"https://elsewhere.example\"; });\nexport {};\n",
        );
        for (case, mut files) in [
            ("g10", forwarded),
            ("g10d", defaulted),
            ("g11", reexported),
            ("g13", named),
            ("g13", namespace),
            ("g13", loaded),
        ] {
            // A clean importer beside it, in the same service.
            files.extend(api_and_reader("clean", "/clean"));
            let (dir, discovery) = discover_owned(&files);
            for reader in [
                format!("src/{case}/reader.ts"),
                "src/clean/reader.ts".to_string(),
            ] {
                let rows = library_rows_of(&dir, &discovery, &reader, &verified_sample());
                assert!(rows.is_empty(), "{case}: {reader}: {rows:#?}");
            }
            assert_eq!(
                library_stated(&dir, &discovery, "src/clean/api.ts"),
                stated(&[(3, "GET", "/clean/own")]),
                "{case}: a declaring module still reads its own calls"
            );
        }
    }

    /// carrick#1568 fix round 2, W1: an import the scan cannot follow to a
    /// file may name an instance under an alias it does not know. A use of
    /// one that would take a client away turns imported reading off: a
    /// tsconfig whose `paths` sit in a referenced project, a bundler-only
    /// alias in a `.js` file, an alias nothing maps, a computed call through
    /// one, and a `require` or `import()` of one. Each declaring module still
    /// reads its own calls.
    #[test]
    fn a_use_through_an_import_the_scan_cannot_follow_turns_imported_reading_off() {
        let write = "(api as any).defaults.baseURL = \"https://elsewhere.example\";\n";
        let cases: [(&str, Vec<(String, String)>); 6] = [
            (
                "re-exported",
                vec![
                    (
                        "src/w/barrel.ts".to_string(),
                        "import { api } from \"@/w/api\";\nexport { api };\n".to_string(),
                    ),
                    (
                        "src/w/boot.ts".to_string(),
                        format!("import {{ api }} from \"./barrel\";\n{write}"),
                    ),
                ],
            ),
            (
                "references",
                vec![
                    (
                        "tsconfig.json".to_string(),
                        "{ \"files\": [], \"references\": [{ \"path\": \"./tsconfig.app.json\" }] }\n"
                            .to_string(),
                    ),
                    (
                        "tsconfig.app.json".to_string(),
                        "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": [\"./src/*\"] } }, \"include\": [\"src\"] }\n"
                            .to_string(),
                    ),
                    (
                        "src/w/boot.ts".to_string(),
                        format!("import {{ api }} from \"@/w/api\";\n{write}"),
                    ),
                ],
            ),
            (
                "bundler alias",
                vec![(
                    "src/w/boot.js".to_string(),
                    "import { api } from \"~w/api\";\napi.defaults.baseURL = \"x\";\n".to_string(),
                )],
            ),
            (
                "unmapped alias",
                vec![(
                    "src/w/boot.ts".to_string(),
                    format!("import {{ api }} from \"@/w/api\";\n{write}"),
                )],
            ),
            (
                "computed call",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import { api } from \"@/w/api\";\ndeclare const name: string;\n(api as any)[name]();\n"
                        .to_string(),
                )],
            ),
            (
                "load",
                vec![(
                    "src/w/boot.ts".to_string(),
                    format!("export async function boot() {{\n  const {{ api }} = await import(\"@/w/api\");\n  {write}}}\n"),
                )],
            ),
        ];
        for (case, extra) in cases {
            let mut files: Vec<(String, String)> = api_and_reader("w", "/w").into();
            files.extend(extra);
            let (dir, discovery) = discover_owned(&files);
            let rows = library_rows_of(&dir, &discovery, "src/w/reader.ts", &verified_sample());
            assert!(rows.is_empty(), "{case}: {rows:#?}");
            assert_eq!(
                library_stated(&dir, &discovery, "src/w/api.ts"),
                stated(&[(3, "GET", "/w/own")]),
                "{case}: the declaring module still reads its own calls"
            );
        }
    }

    /// carrick#1568 fix round 3: a module that names a specifier nothing
    /// resolves, in any position that loads a module at run time, turns
    /// imported reading off, however the binding is then used: each
    /// re-export form through a barrel, a side-effect import, a call by name
    /// (on or off the verified surface), an `instanceof` read, a JSX tag, a
    /// subpath of a builtin's name the runtime does not list (`http/client`)
    /// and a `node:` name that is no builtin. A clean importer beside it
    /// reads nothing either; the declaring module keeps its own calls.
    #[test]
    fn a_module_naming_a_specifier_nothing_resolves_turns_imported_reading_off() {
        let write = "(api as any).defaults.baseURL = \"https://elsewhere.example\";\n";
        let cases: Vec<(&str, Vec<(String, String)>)> = vec![
            (
                "export from",
                vec![
                    ("src/w/barrel.ts".to_string(), "export { api } from \"@/w/api\";\n".to_string()),
                    ("src/w/boot.ts".to_string(), format!("import {{ api }} from \"./barrel\";\n{write}")),
                ],
            ),
            (
                "export star",
                vec![
                    ("src/w/barrel.ts".to_string(), "export * from \"@/w/api\";\n".to_string()),
                    ("src/w/boot.ts".to_string(), format!("import {{ api }} from \"./barrel\";\n{write}")),
                ],
            ),
            (
                "export star as",
                vec![
                    ("src/w/barrel.ts".to_string(), "export * as ns from \"@/w/api\";\n".to_string()),
                    (
                        "src/w/boot.ts".to_string(),
                        "import { ns } from \"./barrel\";\n(ns.api as any).defaults.baseURL = \"x\";\n".to_string(),
                    ),
                ],
            ),
            // A barrel no file imports through: only the specifier it names
            // can say anything.
            (
                "export renamed, barrel alone",
                vec![("src/w/barrel.ts".to_string(), "export { api as client } from \"@/w/api\";\n".to_string())],
            ),
            (
                "export star, barrel alone",
                vec![("src/w/barrel.ts".to_string(), "export * from \"@/w/api\";\n".to_string())],
            ),
            (
                "export star as, barrel alone",
                vec![("src/w/barrel.ts".to_string(), "export * as ns from \"@/w/api\";\n".to_string())],
            ),
            ("side effect", vec![("src/w/boot.ts".to_string(), "import \"@/w/setup\";\n".to_string())]),
            (
                "call by name off the surface",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import { api } from \"@/w/api\";\n(api as any).setBaseURL(\"https://elsewhere.example\");\n".to_string(),
                )],
            ),
            (
                "call by name on the surface",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import { api } from \"@/w/api\";\nexport function f() { return api.get(\"/boot\"); }\n".to_string(),
                )],
            ),
            (
                "instanceof",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import { HttpError } from \"@/w/errors\";\nexport const isErr = (e: unknown): boolean => e instanceof HttpError;\n".to_string(),
                )],
            ),
            (
                "jsx",
                vec![(
                    "src/w/view.tsx".to_string(),
                    "import { Button } from \"@/w/button\";\nexport const v = <Button />;\n".to_string(),
                )],
            ),
            (
                "builtin-named subpath",
                vec![("src/w/boot.ts".to_string(), format!("import {{ api }} from \"http/client\";\n{write}"))],
            ),
            (
                "node: name that is no builtin",
                vec![("src/w/boot.ts".to_string(), format!("import {{ api }} from \"node:w-api\";\n{write}"))],
            ),
            // Positions that name a binding in no expression of their own.
            (
                "import-equals alias",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import * as lib from \"@/w/api\";\nimport client = lib.api;\n(client as any).defaults.baseURL = \"x\";\n".to_string(),
                )],
            ),
            (
                "export import alias",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import * as lib from \"@/w/api\";\nexport import client = lib.api;\n".to_string(),
                )],
            ),
            (
                "class extends a member",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import * as lib from \"@/w/base\";\nexport class S extends lib.Base {}\n".to_string(),
                )],
            ),
            (
                "decorator member chain",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import * as lib from \"@/w/decorators\";\n@lib.tag()\nexport class S {}\n".to_string(),
                )],
            ),
            (
                "jsx member root",
                vec![(
                    "src/w/view.tsx".to_string(),
                    "import * as ui from \"@/w/ui\";\nexport const v = <ui.Button />;\n".to_string(),
                )],
            ),
            (
                "export =",
                vec![(
                    "src/w/boot.ts".to_string(),
                    "import { api } from \"@/w/api\";\nexport = api;\n".to_string(),
                )],
            ),
            (
                "an alias onto a file that is not there",
                vec![
                    ("tsconfig.json".to_string(), PATHS_TSCONFIG.to_string()),
                    ("src/w/boot.ts".to_string(), "import \"@/styles/missing.css\";\n".to_string()),
                ],
            ),
        ];
        for (case, extra) in cases {
            let mut files: Vec<(String, String)> = api_and_reader("w", "/w").into();
            files.extend(api_and_reader("ctl", "/ctl"));
            files.extend(extra);
            let (dir, discovery) = discover_owned(&files);
            for reader in ["src/w/reader.ts", "src/ctl/reader.ts"] {
                let rows = library_rows_of(&dir, &discovery, reader, &verified_sample());
                assert!(rows.is_empty(), "{case}: {reader}: {rows:#?}");
            }
            assert_eq!(
                library_stated(&dir, &discovery, "src/ctl/api.ts"),
                stated(&[(3, "GET", "/ctl/own")]),
                "{case}: the declaring module still reads its own calls"
            );
        }
    }

    /// carrick#1568 fix round 3: what loads nothing, or loads only what the
    /// runtime or a declared package supplies, turns nothing off. `import
    /// type`, `type`-only specifiers, an import used only as a type (which
    /// the compiler erases), exact runtime builtins with and without `node:`
    /// (`fs`, `node:fs`, `fs/promises`, `node:sqlite`), a declared package,
    /// a relative specifier handed to a call that names a directory, an
    /// asset import with a query (`./view.css?inline`), `./helper.js`
    /// naming the `helper.ts` beside it, `import T = ns.Type` used only as a
    /// type, `implements` and an interface's `extends` (types), a package
    /// declared in `devDependencies`, and a `paths` alias that lands on a
    /// `.css` or `.json` file that is there.
    #[test]
    fn a_module_naming_only_erased_imports_builtins_or_declared_packages_turns_nothing_off() {
        let mut files: Vec<(String, String)> = api_and_reader("w", "/w").into();
        files.push((
            "package.json".to_string(),
            "{ \"name\": \"w\", \"dependencies\": { \"@fixture/http\": \"^1.4.0\", \"declared-package\": \"^1.0.0\" }, \"devDependencies\": { \"dev-only\": \"^1.0.0\" } }\n".to_string(),
        ));
        files.push(("tsconfig.json".to_string(), PATHS_TSCONFIG.to_string()));
        files.push((
            "src/styles/globals.css".to_string(),
            "body {}\n".to_string(),
        ));
        files.push(("src/data/x.json".to_string(), "{ \"a\": 1 }\n".to_string()));
        files.push((
            "src/w/types-only.ts".to_string(),
            "import * as unmapped from \"~unmapped/api\";\n\
             import T = unmapped.Api;\n\
             import { Shape } from \"~unmapped/types\";\n\
             import { Base } from \"~unmapped/base\";\n\
             export let t: T | undefined;\n\
             export class S implements Shape { get(p: string): unknown { return p; } }\n\
             export interface Mine extends Base { y: number }\n"
                .to_string(),
        ));
        files.push((
            "src/w/assets.ts".to_string(),
            "import \"@/styles/globals.css\";\n\
             import data from \"@/data/x.json\";\n\
             import { api as devApi } from \"dev-only\";\n\
             (devApi as any).defaults.baseURL = \"x\";\n\
             export const d = data;\n"
                .to_string(),
        ));
        files.push((
            "src/w/boot.ts".to_string(),
            "import type { Api } from \"@/types\";\n\
             import { type Api as A2 } from \"@/types\";\n\
             import { Api as A3 } from \"@/types\";\n\
             export type { Api as A4 } from \"@/types\";\n\
             export { type Api as A5 } from \"@/types\";\n\
             import { EventEmitter } from \"events\";\n\
             import fs from \"node:fs\";\n\
             import { readFile } from \"fs/promises\";\n\
             import { DatabaseSync } from \"node:sqlite\";\n\
             import * as declared from \"declared-package\";\n\
             import styles from \"./view.css?inline\";\n\
             import { helper } from \"./helper.js\";\n\
             declare function serve(dir: string): void;\n\
             export const x: Api | A2 | A3 | undefined = undefined;\n\
             export class Bus extends EventEmitter {}\n\
             export const read = [fs.readFileSync, readFile, DatabaseSync, declared.thing, styles, helper];\n\
             serve(\"./public\");\n"
                .to_string(),
        ));
        files.push((
            "src/w/view.css".to_string(),
            ".a { color: red; }\n".to_string(),
        ));
        files.push((
            "src/w/helper.ts".to_string(),
            "export const helper = 1;\n".to_string(),
        ));
        let (dir, discovery) = discover_owned(&files);
        assert_eq!(
            library_stated(&dir, &discovery, "src/w/reader.ts"),
            stated(&[(2, "GET", "/w/read")])
        );
    }

    /// carrick#1568 fix round 4, Z1: `import client = lib.api` names `lib` in
    /// no expression, and a write through `client` changes what `lib`
    /// publishes. An alias used as a value, or exported, hands its root on,
    /// through a namespace import or a namespace re-export, so no module
    /// reads through the instance; a clean importer beside it still does.
    #[test]
    fn an_import_equals_alias_used_as_a_value_hands_its_root_on() {
        let write = "(client as any).defaults.baseURL = \"https://elsewhere.example\";\n";
        let cases: Vec<(&str, Vec<(String, String)>)> = vec![
            (
                "namespace import",
                vec![(
                    "src/w/boot.ts".to_string(),
                    format!("import * as lib from \"./api\";\nimport client = lib.api;\n{write}"),
                )],
            ),
            (
                "namespace re-export",
                vec![
                    (
                        "src/w/barrel.ts".to_string(),
                        "export * as ns from \"./api\";\n".to_string(),
                    ),
                    (
                        "src/w/boot.ts".to_string(),
                        format!(
                            "import {{ ns }} from \"./barrel\";\nimport client = ns.api;\n{write}"
                        ),
                    ),
                ],
            ),
            (
                "alias of an alias",
                vec![(
                    "src/w/boot.ts".to_string(),
                    format!(
                        "import * as lib from \"./api\";\nimport inner = lib;\nimport client = inner.api;\n{write}"
                    ),
                )],
            ),
        ];
        for (case, extra) in cases {
            let mut files: Vec<(String, String)> = api_and_reader("w", "/w").into();
            files.extend(api_and_reader("ctl", "/ctl"));
            files.extend(extra);
            let (dir, discovery) = discover_owned(&files);
            let rows = library_rows_of(&dir, &discovery, "src/w/reader.ts", &verified_sample());
            assert!(rows.is_empty(), "{case}: {rows:#?}");
            assert_eq!(
                library_stated(&dir, &discovery, "src/ctl/reader.ts"),
                stated(&[(2, "GET", "/ctl/read")]),
                "{case}: a clean importer still reads"
            );
        }
    }

    /// carrick#1568 fix round 3: a barrel outside the service that re-exports
    /// through a specifier nothing resolves answers "unresolved", not
    /// "absent", so an import through it turns imported reading off where no
    /// file of the service names that specifier itself.
    #[test]
    fn an_import_through_an_outside_barrel_that_resolves_nothing_turns_imported_reading_off() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let files: Vec<(String, String)> = [
            ("package.json".to_string(), SAMPLE_MANIFEST.to_string()),
            (
                "other/barrel.ts".to_string(),
                "export { api } from \"@/svc/lib/api\";\n".to_string(),
            ),
            (
                "svc/boot.ts".to_string(),
                "import { api } from \"../other/barrel\";\n(api as any).defaults.baseURL = \"x\";\n"
                    .to_string(),
            ),
        ]
        .into_iter()
        .chain(api_and_reader("lib", "/svc").map(|(name, source)| {
            (
                name.replace("src/lib/", "svc/lib/"),
                source,
            )
        }))
        .collect();
        for (name, source) in &files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
            std::fs::write(&path, source).expect("write");
        }
        let service = Config {
            directory: Some("svc".to_string()),
            ..Config::default()
        };
        let cm: Lrc<SourceMap> = Default::default();
        let discovery =
            discover_files_and_symbols(&dir.path().to_string_lossy(), &service, cm).unwrap();
        let rows = library_rows_of(&dir, &discovery, "svc/lib/reader.ts", &verified_sample());
        assert!(rows.is_empty(), "{rows:#?}");
    }

    /// carrick#1568 fix round 1, F1: a row stated in an importing module
    /// joins a base read in the declaring module's scope only where every
    /// piece means the same in both: text the source writes, or an
    /// environment read. A base naming a binding (an imported config, an
    /// imported constant) would be read by the passes after this one as
    /// whatever the importer binds to that name.
    #[test]
    fn an_imported_instance_states_only_a_base_that_means_the_same_in_the_importer() {
        let (dir, discovery) = discover_service(&[
            (
                "src/q06/config.ts",
                "export const config = { apiUrl: process.env.ORDERS_API_URL };\n",
            ),
            (
                "src/q06/feature-config.ts",
                "export const config = { apiUrl: process.env.BILLING_API_URL };\n",
            ),
            (
                "src/q06/api.ts",
                "import http from \"@fixture/http\";\n\
                 import { config } from \"./config\";\n\
                 export const api = http.create({ baseURL: config.apiUrl });\n\
                 export function own() { return api.get(\"/own\"); }\n",
            ),
            (
                "src/q06/reader.ts",
                "import { api } from \"./api\";\n\
                 import { config } from \"./feature-config\";\n\
                 export const c = config;\n\
                 export function r() { return api.get(\"/read\"); }\n",
            ),
            ("src/q01/config.ts", "export const BASE = \"/q01right\";\n"),
            (
                "src/q01/api.ts",
                "import http from \"@fixture/http\";\n\
                 import { BASE } from \"./config\";\n\
                 export const api = http.create({ baseURL: BASE });\n",
            ),
            (
                "src/q01/reader.ts",
                "import { api } from \"./api\";\n\
                 export function r() { return api.get(\"/read\"); }\n",
            ),
            (
                "src/q04/api.ts",
                "import http from \"@fixture/http\";\n\
                 const BASE = \"/q04right\";\n\
                 export const api = http.create({ baseURL: BASE });\n",
            ),
            (
                "src/q04/reader.ts",
                "import { api } from \"./api\";\n\
                 export function r() { return api.get(\"/read\"); }\n",
            ),
            (
                "src/p12/api.ts",
                "import http from \"@fixture/http\";\n\
                 export const api = http.create({ baseURL: process.env.P12_URL });\n",
            ),
            (
                "src/p12/reader.ts",
                "import { api } from \"./api\";\n\
                 export function r() { return api.get(\"/read\"); }\n",
            ),
        ]);
        for file in ["src/q06/reader.ts", "src/q01/reader.ts"] {
            let rows = library_rows_of(&dir, &discovery, file, &verified_sample());
            assert!(rows.is_empty(), "{file}: {rows:#?}");
        }
        assert_eq!(
            library_rows_of(&dir, &discovery, "src/q06/api.ts", &verified_sample())
                .iter()
                .map(|row| row.target.as_str())
                .collect::<Vec<_>>(),
            vec!["${config.apiUrl}/own"],
            "the declaring module states its own base, which its own scope reads"
        );
        assert_eq!(
            library_stated(&dir, &discovery, "src/q04/reader.ts"),
            stated(&[(2, "GET", "/q04right/read")])
        );
        assert_eq!(
            library_stated(&dir, &discovery, "src/p12/reader.ts"),
            stated(&[(2, "GET", "${process.env.P12_URL}/read")])
        );
    }

    /// carrick#1568 fix round 1, F3 and the type-position gap: a named
    /// function expression's own name is a declaration of that name, so the
    /// import is no client in that file; and a type literal's key named like
    /// the import is no use of it, so it takes nothing away anywhere.
    #[test]
    fn a_function_expression_name_shadows_and_a_type_key_does_not_use() {
        let (dir, discovery) = discover_service(&[
            ("src/p08/api.ts", SHARED_API),
            (
                "src/p08/shadow.ts",
                "import { api } from \"./api\";\n\
                 export function mk() {\n\
                 \x20 const h = function api(): unknown {\n\
                 \x20   // @ts-expect-error the function has no get\n\
                 \x20   return api.get(\"/fnexpr\");\n\
                 \x20 };\n\
                 \x20 return h();\n\
                 }\n",
            ),
            ("src/v02/api.ts", SHARED_API),
            (
                "src/v02/shadow.ts",
                "import { api } from \"./api\";\n\
                 export const f = ({ api }: { api: { get(p: string): string } }): unknown => api.get(\"/destructured\");\n",
            ),
            (
                "src/v02/reader.ts",
                "import { api } from \"./api\";\n\
                 export function r() { return api.get(\"/read\"); }\n",
            ),
        ]);
        let rows = library_rows_of(&dir, &discovery, "src/p08/shadow.ts", &verified_sample());
        assert!(rows.is_empty(), "{rows:#?}");
        assert_eq!(
            library_stated(&dir, &discovery, "src/v02/reader.ts"),
            stated(&[(2, "GET", "/api/v1/read")])
        );
        assert_eq!(
            library_stated(&dir, &discovery, "src/v02/api.ts"),
            stated(&[(5, "GET", "/api/v1/health")])
        );
    }

    /// carrick#1564 re-review, R2: a spread of a constant puts in place
    /// exactly the keys its object literal writes only while the file never
    /// writes through it, passes it to a call or aliases it; otherwise it may
    /// hold any key, on the library path and in a plain request alike.
    #[test]
    fn a_spread_constant_the_file_can_change_states_no_key() {
        let (dir, discovery) = discover_sources(&[
            (
                "src/library.ts",
                "import http from \"@fixture/http\";\n\
                 \n\
                 declare const prod: boolean;\n\
                 const overrides: { baseURL?: string } = {};\n\
                 if (prod) {\n\
                 \x20 overrides.baseURL = \"/prod\";\n\
                 }\n\
                 const api = http.create({ baseURL: \"/dev\", ...overrides });\n\
                 const FIXED = { timeout: 5 };\n\
                 const fixed = http.create({ baseURL: \"/fixed\", ...FIXED });\n\
                 \n\
                 export function n7() { return api.get(\"/mutated-const\"); }\n\
                 export function kept() { return fixed.get(\"/kept\"); }\n",
            ),
            (
                "src/plain.ts",
                "const KNOWN = \"/api/known\";\n\
                 const SENT_URL = \"/api/sent\";\n\
                 const ALIASED_URL = \"/api/aliased\";\n\
                 const WRITTEN_URL = \"/api/written\";\n\
                 const OPTS = { method: \"POST\" };\n\
                 const SENT = { method: \"PUT\" };\n\
                 const ALIASED = { method: \"PATCH\" };\n\
                 const WRITTEN = { method: \"DELETE\" };\n\
                 const alias = ALIASED;\n\
                 WRITTEN.method = \"GET\";\n\
                 declare function prepare(init: object): void;\n\
                 prepare(SENT);\n\
                 \n\
                 export function known() { return fetch(KNOWN, { ...OPTS }); }\n\
                 export function sent() { return fetch(SENT_URL, { ...SENT }); }\n\
                 export function aliased() { return fetch(ALIASED_URL, { ...ALIASED }); }\n\
                 export function written() { return fetch(WRITTEN_URL, { ...WRITTEN }); }\n\
                 \n\
                 const DELETED_URL = \"/api/deleted\";\n\
                 const BUMPED_URL = \"/api/bumped\";\n\
                 const TAKEN_URL = \"/api/taken\";\n\
                 const SHADOWED_URL = \"/api/shadowed\";\n\
                 const DELETED = { method: \"POST\", count: 1 };\n\
                 const BUMPED = { method: \"POST\", count: 1 };\n\
                 const TAKEN = { method: \"POST\" };\n\
                 const SHADOWED = { method: \"POST\" };\n\
                 delete (DELETED as any).method;\n\
                 BUMPED.count++;\n\
                 declare const source: { method: string };\n\
                 ({ method: TAKEN.method } = source);\n\
                 export function deleted() { return fetch(DELETED_URL, { ...DELETED }); }\n\
                 export function bumped() { return fetch(BUMPED_URL, { ...BUMPED }); }\n\
                 export function taken() { return fetch(TAKEN_URL, { ...TAKEN }); }\n\
                 export function shadowed() {\n\
                 \x20 let SHADOWED = { method: \"PUT\" };\n\
                 \x20 return fetch(SHADOWED_URL, { ...SHADOWED });\n\
                 }\n",
            ),
        ]);
        let rows = library_rows_of(&dir, &discovery, "src/library.ts", &verified_sample());
        let stated: Vec<(u32, &str)> = rows
            .iter()
            .map(|row| (row.line, row.target.as_str()))
            .collect();
        assert_eq!(stated, vec![(13, "/fixed/kept")], "{rows:#?}");

        let rows = summary_rows_of(&dir, &discovery, "src/plain.ts");
        let stated: Vec<(u32, &str, &str)> = rows
            .iter()
            .map(|row| (row.line, row.method.as_str(), row.target.as_str()))
            .collect();
        assert_eq!(stated, vec![(14, "POST", "/api/known")], "{rows:#?}");
    }

    /// carrick#1564 review, findings 2 and 4 on the library path: a field a
    /// constructor branch writes again, a field a subclass in the file
    /// declares again, and a static member reading `this` hold no instance;
    /// an instance field read by an instance method still does.
    #[test]
    fn a_field_written_twice_or_read_statically_holds_no_client() {
        let (dir, discovery) = discover_sources(&[(
            "src/fields.ts",
            "import http from \"@fixture/http\";\n\
             \n\
             export class Gateway {\n\
             \x20 private api;\n\
             \x20 constructor(beta: boolean) {\n\
             \x20   this.api = http.create({ baseURL: \"/stable\" });\n\
             \x20   if (beta) {\n\
             \x20     this.api = http.create({ baseURL: \"/beta\" });\n\
             \x20   }\n\
             \x20 }\n\
             \x20 list() { return this.api.get(\"/items\"); }\n\
             }\n\
             \n\
             export class Both {\n\
             \x20 static api = http.create({ baseURL: \"/static\" });\n\
             \x20 api = http.create({ baseURL: \"/instance\" });\n\
             \x20 static load() { return this.api.get(\"/loaded\"); }\n\
             \x20 run() { return this.api.get(\"/ran\"); }\n\
             }\n\
             \n\
             export class BaseApi {\n\
             \x20 protected api = http.create({ baseURL: \"/default\" });\n\
             \x20 list() { return this.api.get(\"/items\"); }\n\
             }\n\
             \n\
             export class UsersApi extends BaseApi {\n\
             \x20 protected api = http.create({ baseURL: \"/users-svc\" });\n\
             }\n",
        )]);
        let rows = library_rows_of(&dir, &discovery, "src/fields.ts", &verified_sample());
        let stated: Vec<(u32, &str)> = rows
            .iter()
            .map(|row| (row.line, row.target.as_str()))
            .collect();
        assert_eq!(stated, vec![(18, "/instance/ran")], "{rows:#?}");
    }

    /// The review's Phase 1 shapes with no library at all (carrick#1564
    /// review, findings 1 and 2, in code carrick#1555 shipped): a method or a
    /// body key written before a spread the source cannot read, a URL field
    /// a constructor branch writes again, a URL field a subclass redeclares,
    /// and a static method reading `this.url`. None states a row, while a key
    /// written after a spread, and a spread whose keys the source states,
    /// keep theirs.
    #[test]
    fn a_value_the_source_may_overwrite_states_nothing_in_a_request_summary() {
        let (dir, discovery) = discover_sources(&[(
            "src/phase1.ts",
            "const BASE = process.env.API_URL;\n\
             \n\
             export class Sender {\n\
             \x20 private url = `${BASE}/rpc`;\n\
             \x20 before(opts: object) { return fetch(this.url, { method: \"POST\", ...opts }); }\n\
             \x20 after(opts: object) { return fetch(this.url, { ...opts, method: \"PUT\" }); }\n\
             \x20 body(extra: object) {\n\
             \x20   return fetch(this.url, { method: \"POST\", body: JSON.stringify({ action: \"sync\", ...extra }) });\n\
             \x20 }\n\
             \x20 known(flag: boolean) {\n\
             \x20   return fetch(this.url, { method: \"POST\", body: JSON.stringify({ action: \"poll\", ...(flag ? { limit: 1 } : {}) }) });\n\
             \x20 }\n\
             \x20 spreadOnly(opts: object) { return fetch(this.url, { ...opts }); }\n\
             }\n\
             \n\
             export class Branchy {\n\
             \x20 private url: string;\n\
             \x20 constructor(beta: boolean) {\n\
             \x20   this.url = `${BASE}/v1/items`;\n\
             \x20   if (beta) {\n\
             \x20     this.url = `${BASE}/beta/items`;\n\
             \x20   }\n\
             \x20 }\n\
             \x20 list() { return fetch(this.url, { method: \"GET\" }); }\n\
             }\n\
             \n\
             export class Parent {\n\
             \x20 protected url = `${BASE}/parent/items`;\n\
             \x20 list() { return fetch(this.url, { method: \"GET\" }); }\n\
             }\n\
             \n\
             export class Child extends Parent {\n\
             \x20 protected url = `${BASE}/child/items`;\n\
             }\n\
             \n\
             export class Statics {\n\
             \x20 url = `${BASE}/instance/items`;\n\
             \x20 static load() { return fetch(this.url, { method: \"GET\" }); }\n\
             }\n",
        )]);
        let rows = summary_rows_of(&dir, &discovery, "src/phase1.ts");
        let stated: Vec<(u32, &str, &str, Option<&str>)> = rows
            .iter()
            .map(|row| {
                (
                    row.line,
                    row.method.as_str(),
                    row.target.as_str(),
                    row.body_literals.get("action").map(String::as_str),
                )
            })
            .collect();
        assert_eq!(
            stated,
            vec![
                (6, "PUT", "${process.env.API_URL}/rpc", None),
                (8, "POST", "${process.env.API_URL}/rpc", None),
                (11, "POST", "${process.env.API_URL}/rpc", Some("poll")),
            ],
            "{rows:#?}"
        );
    }

    /// The contract sample's entries: `fixture-slow-http` pending.
    fn sample_entries() -> Vec<crate::client_semantics::ClientSemanticsEntry> {
        let detection: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .unwrap();
        detection.client_semantics.unwrap()
    }

    /// carrick#1564, third review: a scan that stops before it reads the
    /// summaries drops the schedule, and the dropped schedule sends no
    /// further ask and prints no further line.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_schedule_asks_nothing_further() {
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (counted, printed) = (asks.clone(), lines.clone());
        let settling = spawn_schedule(
            sample_entries(),
            |_| true,
            crate::client_semantics::PENDING_REASK_WAITS.to_vec(),
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { None }
            },
            move |notice| printed.lock().unwrap().push(notice.line()),
        );
        // The task prints its first line and sleeps out the first wait.
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(lines.lock().unwrap().len(), 1, "the schedule started");
        drop(settling);
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            lines.lock().unwrap().len(),
            1,
            "{:?}",
            lines.lock().unwrap()
        );
    }

    fn broken_ask() -> Option<Vec<crate::client_semantics::ClientSemanticsEntry>> {
        panic!("the schedule broke")
    }

    /// carrick#1564, third review: a panic inside the schedule is the
    /// schedule's own. It is raised inside a quiet poll, which the
    /// process-wide hook neither reports as the scan failing nor prints
    /// (`tests/panic_hook_test.rs` proves that half), and the scan keeps what
    /// the first ask gave.
    #[tokio::test]
    async fn a_panic_in_the_schedule_is_quiet_and_keeps_the_first_answer() {
        let quiet = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = quiet.clone();
        let asked = sample_entries();
        let settling = spawn_schedule(
            asked.clone(),
            |_| true,
            vec![std::time::Duration::ZERO; 2],
            move || {
                seen.store(
                    crate::panic_report::in_quiet_poll(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                async { broken_ask() }
            },
            |_| {},
        );
        assert_eq!(settling.settled().await, Some(asked));
        assert!(
            quiet.load(std::sync::atomic::Ordering::SeqCst),
            "the ask ran inside a quiet poll"
        );
    }

    /// carrick#1564 re-review, R4: the first re-ask, made before the analysis
    /// for a stored detection, tells the user what it waits for and how long,
    /// and gives up after `PENDING_REASK_TIMEOUT`, keeping the stored
    /// detection. A detection never asked names the libraries it describes.
    #[tokio::test(start_paused = true)]
    async fn the_first_reask_says_what_it_waits_for_and_gives_up_in_time() {
        let root = tempfile::tempdir().unwrap();
        for package in ["fixture-slow-http", "@fixture/http"] {
            let manifest = root
                .path()
                .join("node_modules")
                .join(package)
                .join("package.json");
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            std::fs::write(&manifest, "{}").unwrap();
        }
        let cached: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .unwrap();
        let never_answers =
            || std::future::pending::<Result<DetectionResult, Box<dyn std::error::Error>>>();

        let mut lines = Vec::new();
        let started = tokio::time::Instant::now();
        let kept = tokio::time::timeout(
            crate::client_semantics::PENDING_REASK_TIMEOUT * 2,
            reask_client_semantics(
                &cached,
                PreviousGeneration::Stored,
                root.path(),
                root.path(),
                never_answers,
                |notice| lines.push(notice.line()),
            ),
        )
        .await
        .expect("the re-ask gives up within its bound");
        assert_eq!(
            started.elapsed(),
            crate::client_semantics::PENDING_REASK_TIMEOUT
        );
        assert_eq!(
            serde_json::to_value(&kept).unwrap(),
            serde_json::to_value(&cached).unwrap()
        );
        assert_eq!(
            lines,
            vec!["1 of 3 client libraries still being described, waiting up to 30 s"]
        );

        let never_asked = DetectionResult {
            client_semantics: None,
            ..cached.clone()
        };
        let mut lines = Vec::new();
        let _ = tokio::time::timeout(
            crate::client_semantics::PENDING_REASK_TIMEOUT * 2,
            reask_client_semantics(
                &never_asked,
                PreviousGeneration::Stored,
                root.path(),
                root.path(),
                never_answers,
                |notice| lines.push(notice.line()),
            ),
        )
        .await
        .expect("the re-ask gives up within its bound");
        assert_eq!(
            lines,
            vec!["2 client libraries being described, waiting up to 30 s"]
        );
    }

    /// What a re-ask said and did, in order.
    #[derive(Clone, Default)]
    struct Said(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    /// A notice that records its own end when it is dropped, which is how
    /// [`crate::progress::WaitNotice`] states it.
    struct SaidUntilDropped(Said, String);

    impl Drop for SaidUntilDropped {
        fn drop(&mut self) {
            self.0.push(format!("ended: {}", self.1));
        }
    }

    impl Said {
        fn push(&self, line: String) {
            self.0.borrow_mut().push(line);
        }

        fn lines(&self) -> Vec<String> {
            self.0.borrow().clone()
        }

        fn notice(&self, notice: crate::client_semantics::ScheduleNotice) -> SaidUntilDropped {
            self.push(format!("shown: {}", notice.line()));
            SaidUntilDropped(self.clone(), notice.line())
        }
    }

    /// carrick#1674: the re-ask's notice ends once the ask is over, whether
    /// it answered, failed or ran out of time, and not before. It used to
    /// stay beside the counts until the next service's notice replaced it.
    #[tokio::test(start_paused = true)]
    async fn the_reask_notice_ends_once_the_ask_is_over() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root
            .path()
            .join("node_modules/@fixture/http")
            .join("package.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "{}").unwrap();
        let (stored, answer) = stored_and_other_answer();
        let waiting = "1 client library being described, waiting up to 30 s";

        let said = Said::default();
        let (asked, fresh) = (said.clone(), answer.clone());
        reask_client_semantics(
            &stored,
            PreviousGeneration::Stored,
            root.path(),
            root.path(),
            || async move {
                asked.push("answered".to_string());
                Ok(fresh)
            },
            |notice| said.notice(notice),
        )
        .await;
        assert_eq!(
            said.lines(),
            vec![
                format!("shown: {waiting}"),
                "answered".to_string(),
                format!("ended: {waiting}")
            ]
        );

        let said = Said::default();
        let asked = said.clone();
        reask_client_semantics(
            &stored,
            PreviousGeneration::Stored,
            root.path(),
            root.path(),
            || async move {
                asked.push("failed".to_string());
                Err::<DetectionResult, Box<dyn std::error::Error>>("unreachable".into())
            },
            |notice| said.notice(notice),
        )
        .await;
        assert_eq!(
            said.lines(),
            vec![
                format!("shown: {waiting}"),
                "failed".to_string(),
                format!("ended: {waiting}")
            ]
        );

        let said = Said::default();
        let started = tokio::time::Instant::now();
        reask_client_semantics(
            &stored,
            PreviousGeneration::Stored,
            root.path(),
            root.path(),
            std::future::pending::<Result<DetectionResult, Box<dyn std::error::Error>>>,
            |notice| said.notice(notice),
        )
        .await;
        assert_eq!(
            started.elapsed(),
            crate::client_semantics::PENDING_REASK_TIMEOUT
        );
        assert_eq!(
            said.lines(),
            vec![format!("shown: {waiting}"), format!("ended: {waiting}")]
        );
    }

    /// A stored detection and an answer that names other packages in its
    /// lists and says something else in its notes, both from the contract
    /// sample.
    fn stored_and_other_answer() -> (DetectionResult, DetectionResult) {
        let answer: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .unwrap();
        let stored = DetectionResult {
            frameworks: vec!["fixture-server".to_string()],
            data_fetchers: vec![
                "@fixture/http".to_string(),
                "fixture-stored-only-http".to_string(),
            ],
            messaging_clients: vec!["fixture-queue".to_string()],
            socket_clients: Vec::new(),
            notes: "the stored notes".to_string(),
            client_semantics: None,
        };
        let answer = DetectionResult {
            notes: "the fresh notes".to_string(),
            ..answer
        };
        assert!(!answer.same_lists(&stored), "the premise: other lists");
        (stored, answer)
    }

    /// carrick#1606: the re-ask asks for library semantics, so it takes the
    /// answer's semantics and nothing else. The stored lists and notes stand
    /// whatever lists the answer names: they move only when a manifest moves
    /// (the `package_json_hash` gate), and so nothing keyed on them is asked
    /// again.
    #[tokio::test]
    async fn a_reask_naming_other_lists_keeps_the_stored_detection_and_takes_its_semantics() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root
            .path()
            .join("node_modules/@fixture/http")
            .join("package.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "{}").unwrap();
        let (stored, answer) = stored_and_other_answer();
        let asked = answer.clone();

        let kept = reask_client_semantics(
            &stored,
            PreviousGeneration::Stored,
            root.path(),
            root.path(),
            || async move { Ok(asked) },
            |_| {},
        )
        .await;
        assert_eq!(
            serde_json::to_value(&kept).unwrap(),
            serde_json::to_value(DetectionResult {
                client_semantics: answer.client_semantics.clone(),
                ..stored
            })
            .unwrap()
        );
    }

    /// carrick#1606: an in-scan re-ask whose answer names other lists still
    /// gives its semantics. `settle_pending` fills only entries still
    /// `pending`, by package name, so nothing the answer adds or omits moves
    /// anything else.
    #[tokio::test]
    async fn an_in_scan_reask_naming_other_lists_gives_its_semantics() {
        let (_, answer) = stored_and_other_answer();
        let semantics =
            semantics_from_reask(std::future::ready(Ok::<_, String>(answer.clone()))).await;
        assert_eq!(semantics, answer.client_semantics);
    }

    /// carrick#1564: an instance is read only when its factory's own claim
    /// verified, whatever the instance's other claims came back as; the
    /// export's claims stand on their own.
    #[test]
    fn an_instance_whose_factory_did_not_verify_states_nothing() {
        let detection: DetectionResult = serde_json::from_str(include_str!(
            "../../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
        ))
        .unwrap();
        let entries = detection.client_semantics.unwrap();
        let derived = crate::client_semantics::derive_claims(&entries);
        let results: Vec<crate::services::type_sidecar::SemanticsResult> = derived
            .checks()
            .into_iter()
            .map(|check| crate::services::type_sidecar::SemanticsResult {
                verdict: if check.claim_id.ends_with(":factory:create") {
                    crate::services::type_sidecar::SemanticsVerdict::Failed
                } else {
                    crate::services::type_sidecar::SemanticsVerdict::Verified
                },
                reason: None,
                claim_id: check.claim_id,
                receiver: check.receiver,
            })
            .collect();
        let semantics =
            crate::client_semantics::LibrarySemantics::from_verdicts(&derived, &results);

        let (dir, discovery) = discover_sources(&[("src/api.ts", HTTP_CLIENT)]);
        let rows = library_rows_of(&dir, &discovery, "src/api.ts", &semantics);
        let lines: Vec<u32> = rows.iter().map(|row| row.line).collect();
        assert_eq!(
            lines,
            vec![23],
            "only the export's own request states a row: {rows:#?}"
        );
    }

    #[test]
    fn discovery_rekeys_same_named_definitions_with_a_relative_path() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for (name, source) in [
            (
                "login/mfa.ts",
                "export class MFAController {\n  post(url: string) {\n    return url;\n  }\n}\n",
            ),
            (
                "register/mfa.ts",
                "export class MFAController {\n  post(user: string) {\n    return user;\n  }\n}\n",
            ),
        ] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
            std::fs::write(&path, source).expect("write");
        }

        let cm: Lrc<SourceMap> = Default::default();
        let definitions =
            discover_files_and_symbols(&dir.path().to_string_lossy(), &Config::default(), cm)
                .unwrap()
                .function_definitions;

        let mut keys: Vec<&String> = definitions.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "MFAController.post@login/mfa.ts",
                "MFAController.post@register/mfa.ts"
            ],
            "both rows survive, each qualified by its repo-relative path"
        );
    }

    // -----------------------------------------------------------------------
    // Type-state enrichment / placeholder handling (#235)
    // -----------------------------------------------------------------------

    fn consumer_entry(type_alias: &str) -> TypeManifestEntry {
        let evidence = TypeEvidence {
            file_path: "lib/api.ts".to_string(),
            span_start: None,
            span_end: None,
            line_number: 5,
            infer_kind: InferKind::CallResult,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
        };
        TypeManifestEntry {
            key: OperationKey::http("GET", "/orders/:id"),
            role: ManifestRole::Consumer,
            type_kind: ManifestTypeKind::Response,
            type_alias: type_alias.to_string(),
            file_path: "lib/api.ts".to_string(),
            line_number: 5,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
            evidence,
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            unwidened_definition: None,
            v1_state_before_demotion: None,
        }
    }

    fn empty_resolution() -> TypeResolutionResult {
        TypeResolutionResult {
            dts_content: None,
            explicit_manifest: vec![],
            inferred_types: vec![],
            symbol_failures: vec![],
            errors: vec![],
            anchor_changes: vec![],
        }
    }

    // -----------------------------------------------------------------
    // carrick#376: per-field `any` provenance reaches the manifest entry
    // -----------------------------------------------------------------

    fn provenance(path: &str, reason: &str) -> crate::services::type_sidecar::TypeProvenance {
        crate::services::type_sidecar::TypeProvenance {
            path: path.to_string(),
            kind: "any".to_string(),
            reason: reason.to_string(),
            detail: Some("why".to_string()),
        }
    }

    /// The capture self-check's findings reach the entry a reader sees, and
    /// they reach it even when the entry has no printed type at all: "no type
    /// here, and here is why" is the answer, and an Unknown entry is exactly
    /// the one the definition resolution skips.
    #[test]
    fn capture_provenance_joins_the_manifest_by_alias() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("carrick-manifest.json"),
            serde_json::json!({
                "aliases": [{
                    "alias": "OrderView",
                    "anchor_kind": "infer",
                    "source_file": "lib/api.ts",
                    "anchor_origin": "deterministic-infer",
                    "serialization": "structural_fallback",
                    "self_check": "decayed_internal",
                    "top_type_at_self_check": false,
                    "any_provenance": [
                        { "path": "meta", "kind": "any", "reason": "declared", "detail": "d" }
                    ]
                }]
            })
            .to_string(),
        )
        .expect("write manifest");

        let mut manifest = vec![consumer_entry("OrderView"), consumer_entry("Untouched")];
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
        stamp_capture_provenance(&mut manifest, &read_capture_records(dir.path()));

        assert_eq!(manifest[0].any_provenance.len(), 1);
        assert_eq!(manifest[0].any_provenance[0].path, "meta");
        assert_eq!(manifest[0].any_provenance[0].reason, "declared");
        assert!(
            manifest[1].any_provenance.is_empty(),
            "an alias with no capture record must be left alone, not blanked"
        );
    }

    /// Two layers report here and neither subsumes the other, so both are
    /// kept — but a repeated finding must not double up and the order must not
    /// depend on which layer ran first, or scan-twice byte-identity fails.
    #[test]
    fn provenance_merge_is_deduped_and_order_independent() {
        let inferrer = provenance("", "no_payload_evidence");
        let capture_a = provenance("meta", "declared");
        let capture_b = provenance("items<0>.id", "declared");

        let mut one = Vec::new();
        merge_any_provenance(&mut one, [inferrer.clone()]);
        merge_any_provenance(&mut one, [capture_a.clone(), capture_b.clone()]);

        let mut other = Vec::new();
        merge_any_provenance(&mut other, [capture_b, capture_a]);
        merge_any_provenance(&mut other, [inferrer.clone()]);

        assert_eq!(one, other, "merge order must not change the published list");
        assert_eq!(
            one.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            vec!["", "items<0>.id", "meta"]
        );

        merge_any_provenance(&mut one, [inferrer]);
        assert_eq!(one.len(), 3, "the same finding must not be published twice");
    }

    /// A genuine shape resolved by the sidecar promotes the entry to Implicit.
    #[test]
    fn enrich_promotes_resolved_consumer_shape() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        resolution.inferred_types.push(InferredType {
            alias: "OrderView".to_string(),
            type_string: "{ id: string; currency: string }".to_string(),
            is_explicit: false,
            source_location: SourceLocation {
                file_path: "lib/api.ts".to_string(),
                start_line: 5,
                end_line: 5,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::CallResult,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
            unwidened_type_string: None,
            stated_body: None,
        });

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(manifest[0].type_state, ManifestTypeState::Implicit);
    }

    /// A consumer alias that only resolves to `unknown` must stay `Unknown` so
    /// the placeholder gate stays shut and the edge reads unverifiable, not
    /// compatible (#235).
    #[test]
    fn enrich_keeps_unknown_resolution_unknown() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        resolution.inferred_types.push(InferredType {
            alias: "OrderView".to_string(),
            type_string: "unknown".to_string(),
            is_explicit: false,
            source_location: SourceLocation {
                file_path: "lib/api.ts".to_string(),
                start_line: 5,
                end_line: 5,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::CallResult,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
            unwidened_type_string: None,
            stated_body: None,
        });

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
    }

    /// An inferred type carrying a deterministic symbol, keyed by the same
    /// `alias` as the manifest entry it enriches — the join the anchor fill
    /// uses. `type_string` is a resolved object, so only the anchor (not the
    /// type-state path) is under test here.
    fn inferred_with_symbol(symbol: &str) -> InferredType {
        InferredType {
            // Matches `consumer_entry("OrderView").type_alias`, so the anchor
            // join (by alias) fires.
            alias: "OrderView".to_string(),
            type_string: "{ id: string; currency: string }".to_string(),
            is_explicit: false,
            source_location: SourceLocation {
                file_path: "lib/api.ts".to_string(),
                start_line: 5,
                end_line: 5,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::ResponseBody,
            primary_type_symbol: Some(symbol.to_string()),
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
            unwidened_type_string: None,
            stated_body: None,
        }
    }

    /// #240: the deterministic anchor fills `primary_type_symbol` from the
    /// inferred symbol, joined by `alias`, when the LLM left it None.
    #[test]
    fn enrich_fills_anchor_from_inferred_symbol_when_llm_none() {
        let mut manifest = vec![consumer_entry("OrderView")];
        // The LLM stamped nothing onto this op.
        assert_eq!(manifest[0].primary_type_symbol, None);

        let mut resolution = empty_resolution();
        resolution
            .inferred_types
            .push(inferred_with_symbol("OrderView"));

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(
            manifest[0].primary_type_symbol.as_deref(),
            Some("OrderView"),
            "anchor must be filled from the inferred symbol when the LLM left it None"
        );
    }

    /// #240: an op the LLM already anchored correctly must NOT be overwritten by
    /// the inferred symbol — the deterministic fill is None-only so POST
    /// /payments and socket ops keep their model-emitted symbol.
    #[test]
    fn enrich_does_not_override_existing_llm_anchor() {
        let mut manifest = vec![consumer_entry("OrderView")];
        // The LLM already stamped the real symbol for this op.
        manifest[0].primary_type_symbol = Some("Payment".to_string());

        let mut resolution = empty_resolution();
        // The sidecar inferred a DIFFERENT symbol at the same location.
        resolution
            .inferred_types
            .push(inferred_with_symbol("OrderView"));

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(
            manifest[0].primary_type_symbol.as_deref(),
            Some("Payment"),
            "an existing LLM anchor must never be regressed by the inferred symbol"
        );
    }

    /// carrick#1779: a `fetch` call's inference anchors on the call's own type,
    /// `Response`, while its text is the body the source reads out of it. The
    /// transport is not the row's type, so it fills no anchor.
    #[test]
    fn enrich_never_fills_an_anchor_with_transport_machinery() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        resolution
            .inferred_types
            .push(inferred_with_symbol("Response"));

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(manifest[0].primary_type_symbol, None);
    }

    // -----------------------------------------------------------------
    // carrick#1779: a rejected model anchor leaves the manifest row
    // -----------------------------------------------------------------

    /// One entry of a consumer call at `file:line`, stamped with the model's
    /// `symbol` and the home the stamp found for it.
    fn stamped_entry(
        alias: &str,
        type_kind: ManifestTypeKind,
        path: &str,
        line_number: u32,
        symbol: &str,
    ) -> TypeManifestEntry {
        let mut entry = consumer_entry(alias);
        entry.key = OperationKey::http("GET", path);
        entry.type_kind = type_kind;
        entry.file_path = "src/api.ts".to_string();
        entry.line_number = line_number;
        entry.primary_type_symbol = Some(symbol.to_string());
        entry.defined_in = Some(crate::cloud_storage::TypeHome {
            file_path: "src/types.ts".to_string(),
            line_number: 1,
            symbol: symbol.to_string(),
        });
        entry
    }

    fn dropped(alias: &str, rejected: &str) -> crate::services::type_sidecar::AnchorChange {
        crate::services::type_sidecar::AnchorChange {
            alias: alias.to_string(),
            rejected: rejected.to_string(),
            reaimed: None,
        }
    }

    /// The source casts the body it reads to a wrapper and the model named the
    /// element: the arbitration dropped the model's symbol, so neither entry
    /// of that call may keep it or its home, and enrichment then anchors the
    /// response from its own inference. Another op on the same line, and the
    /// same op at another site, are not that call.
    #[test]
    fn restamp_lets_go_of_a_dropped_model_anchor() {
        let mut manifest = vec![
            stamped_entry(
                "Members_Response_Call1",
                ManifestTypeKind::Response,
                "/members",
                5,
                "Member",
            ),
            stamped_entry(
                "Members_Request_Call1",
                ManifestTypeKind::Request,
                "/members",
                5,
                "Member",
            ),
            stamped_entry(
                "Teams_Response_Call1",
                ManifestTypeKind::Response,
                "/teams",
                5,
                "Member",
            ),
            stamped_entry(
                "Members_Response_Call2",
                ManifestTypeKind::Response,
                "/members",
                9,
                "Member",
            ),
        ];
        let mut elsewhere = stamped_entry(
            "Members_Response_Call3",
            ManifestTypeKind::Response,
            "/members",
            5,
            "Member",
        );
        elsewhere.file_path = "src/other.ts".to_string();
        let mut producer = stamped_entry(
            "Members_Producer_Response",
            ManifestTypeKind::Response,
            "/members",
            5,
            "Member",
        );
        producer.role = ManifestRole::Producer;
        manifest.extend([elsewhere, producer]);
        let mut resolution = empty_resolution();
        resolution.anchor_changes = vec![dropped("Members_Response_Call1", "Member")];
        let mut inferred = inferred_with_symbol("MembersPage");
        inferred.alias = "Members_Response_Call1".to_string();
        resolution.inferred_types.push(inferred);

        restamp_arbitrated_anchors(&mut manifest, &resolution.anchor_changes, "/repo");
        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(
            manifest[0].primary_type_symbol.as_deref(),
            Some("MembersPage"),
            "the response is anchored by its own inference"
        );
        assert_eq!(
            manifest[0].defined_in, None,
            "no home for a symbol it never had"
        );
        assert_eq!(
            manifest[1].primary_type_symbol, None,
            "the request entry lets go too"
        );
        assert_eq!(manifest[1].defined_in, None);
        for untouched in &manifest[2..] {
            assert_eq!(
                untouched.primary_type_symbol.as_deref(),
                Some("Member"),
                "{}",
                untouched.type_alias
            );
            assert!(untouched.defined_in.is_some());
        }
    }

    /// The stamp keeps one symbol per line, so the call's entries can carry a
    /// symbol another row on that line stated. The arbitration rejected the
    /// call's own symbol, not that one, and it stays.
    #[test]
    fn restamp_leaves_a_symbol_it_did_not_reject() {
        let mut manifest = vec![stamped_entry(
            "Members_Response_Call1",
            ManifestTypeKind::Response,
            "/members",
            5,
            "Order",
        )];

        restamp_arbitrated_anchors(
            &mut manifest,
            &[dropped("Members_Response_Call1", "Member")],
            "/repo",
        );

        assert_eq!(manifest[0].primary_type_symbol.as_deref(), Some("Order"));
        assert!(manifest[0].defined_in.is_some());
    }

    /// A re-aimed request names the root the source states, so both entries of
    /// the call name it too, with the declaration the sidecar found. The
    /// sidecar spells the path in its resolved form (`/private/var/...` for a
    /// macOS temp dir), which the repo root as the scan holds it may not.
    #[test]
    fn restamp_names_a_reaimed_root_at_its_declaration() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(
            repo.path().join("src/pages.ts"),
            "import { Member } from './types';\n\nexport interface MemberPage {\n  members: Member[];\n}\n",
        )
        .expect("write");
        let declaring = repo
            .path()
            .join("src/pages.ts")
            .canonicalize()
            .expect("canonical");
        let mut manifest = vec![
            stamped_entry(
                "Page_Response_Call1",
                ManifestTypeKind::Response,
                "/members",
                5,
                "Member",
            ),
            stamped_entry(
                "Page_Request_Call1",
                ManifestTypeKind::Request,
                "/members",
                5,
                "Member",
            ),
        ];
        let changes = vec![crate::services::type_sidecar::AnchorChange {
            alias: "Page_Response_Call1".to_string(),
            rejected: "Member".to_string(),
            reaimed: Some(crate::services::type_sidecar::AnchorRoot {
                symbol: "MemberPage".to_string(),
                source_file: declaring.to_string_lossy().into_owned(),
            }),
        }];

        restamp_arbitrated_anchors(&mut manifest, &changes, &repo.path().to_string_lossy());

        let home = crate::cloud_storage::TypeHome {
            file_path: "src/pages.ts".to_string(),
            line_number: 3,
            symbol: "MemberPage".to_string(),
        };
        for entry in &manifest {
            assert_eq!(entry.primary_type_symbol.as_deref(), Some("MemberPage"));
            assert_eq!(entry.defined_in.as_ref(), Some(&home));
        }
    }

    /// A root the sidecar found outside the repo is still the anchor, but the
    /// manifest states no home for it: a machine path is not a repo file.
    #[test]
    fn restamp_states_no_home_outside_the_repo() {
        let repo = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let declaring = elsewhere.path().join("pages.ts");
        std::fs::write(
            &declaring,
            "export interface MemberPage {\n  id: string;\n}\n",
        )
        .expect("write");
        let mut manifest = vec![stamped_entry(
            "Page_Response_Call1",
            ManifestTypeKind::Response,
            "/members",
            5,
            "Member",
        )];
        let changes = vec![crate::services::type_sidecar::AnchorChange {
            alias: "Page_Response_Call1".to_string(),
            rejected: "Member".to_string(),
            reaimed: Some(crate::services::type_sidecar::AnchorRoot {
                symbol: "MemberPage".to_string(),
                source_file: declaring.to_string_lossy().into_owned(),
            }),
        }];

        restamp_arbitrated_anchors(&mut manifest, &changes, &repo.path().to_string_lossy());

        assert_eq!(
            manifest[0].primary_type_symbol.as_deref(),
            Some("MemberPage")
        );
        assert_eq!(manifest[0].defined_in, None);

        // An empty root strips nothing from an absolute path, which is no
        // more a repo file than the one above.
        manifest[0].primary_type_symbol = Some("Member".to_string());
        restamp_arbitrated_anchors(&mut manifest, &changes, "");
        assert_eq!(manifest[0].defined_in, None);
    }

    // -----------------------------------------------------------------
    // carrick#1817: a row is anchored at the body its source states
    // -----------------------------------------------------------------

    /// The calling file of the #1817 fixtures: the page type is declared
    /// there, unexported, the way a client often keeps the shape of one read.
    const STATED_SOURCE: &str = "type MemberPage = {\n  members: { id: string }[];\n};\n\nexport async function members() {\n  const response = await fetch('/members');\n  return (await response.json()) as MemberPage;\n}\n";

    /// A repo holding `src/api.ts` (`STATED_SOURCE`), and that file's path as
    /// the sidecar spells it (resolved, `/private/var/...` on macOS).
    fn stated_repo() -> (tempfile::TempDir, String) {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(repo.path().join("src/api.ts"), STATED_SOURCE).expect("write");
        let declaring = repo
            .path()
            .join("src/api.ts")
            .canonicalize()
            .expect("canonical");
        (repo, declaring.to_string_lossy().into_owned())
    }

    /// The call's inference as the sidecar reports a stated body read: its
    /// text is the statement, its own anchor is the call's type (`Response`).
    fn stated_call(alias: &str, stated: crate::services::type_sidecar::StatedBody) -> InferredType {
        let mut inferred = inferred_with_symbol("Response");
        inferred.alias = alias.to_string();
        inferred.infer_kind = InferKind::CallResult;
        inferred.type_string = "{ members: { id: string; }[]; }".to_string();
        inferred.is_explicit = true;
        inferred.stated_body = Some(stated);
        inferred
    }

    fn stated_root(root: &str, source: &str) -> crate::services::type_sidecar::StatedBody {
        crate::services::type_sidecar::StatedBody {
            root: Some(root.to_string()),
            root_source: Some(source.to_string()),
            array_depth: None,
            root_type_arguments: None,
        }
    }

    /// The model named the element inside the page and the arbitration
    /// dropped it (the ticket's case), or the model named nothing: either
    /// way the response entry is anchored at the type the source states the
    /// body to be, with its declaration. The request entry is another alias
    /// with no statement, and the fill's `Response` never reaches either.
    #[test]
    fn stated_root_anchors_a_response_left_without_one() {
        let (repo, declaring) = stated_repo();
        let mut dropped_call = consumer_entry("Members_Response_Call1");
        dropped_call.primary_type_symbol = Some("Member".to_string());
        let mut manifest = vec![
            dropped_call,
            consumer_entry("Members_Response_Call2"),
            consumer_entry("Members_Request_Call2"),
        ];
        manifest[2].type_kind = ManifestTypeKind::Request;
        let mut resolution = empty_resolution();
        resolution.anchor_changes = vec![dropped("Members_Response_Call1", "Member")];
        resolution.inferred_types = vec![
            stated_call(
                "Members_Response_Call1",
                stated_root("MemberPage", &declaring),
            ),
            stated_call(
                "Members_Response_Call2",
                stated_root("MemberPage", &declaring),
            ),
        ];

        let repo_path = repo.path().to_string_lossy();
        restamp_arbitrated_anchors(&mut manifest, &resolution.anchor_changes, &repo_path);
        anchor_stated_body_roots(&mut manifest, &resolution.inferred_types, &repo_path);
        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        let home = crate::cloud_storage::TypeHome {
            file_path: "src/api.ts".to_string(),
            line_number: 1,
            symbol: "MemberPage".to_string(),
        };
        for entry in &manifest[..2] {
            assert_eq!(
                entry.primary_type_symbol.as_deref(),
                Some("MemberPage"),
                "{}",
                entry.type_alias
            );
            assert_eq!(
                entry.defined_in.as_ref(),
                Some(&home),
                "{}",
                entry.type_alias
            );
        }
        assert_eq!(manifest[2].primary_type_symbol, None);
        assert_eq!(manifest[2].defined_in, None);
    }

    /// An anchor the row already has is not the stated root's to replace:
    /// the model's, kept because it is the root or because several requests
    /// fan in to the alias, and a re-aimed root.
    #[test]
    fn stated_root_leaves_an_anchor_in_place() {
        let (repo, declaring) = stated_repo();
        let mut manifest = vec![consumer_entry("Members_Response_Call1")];
        manifest[0].primary_type_symbol = Some("Member".to_string());
        let inferred = vec![stated_call(
            "Members_Response_Call1",
            stated_root("MemberPage", &declaring),
        )];

        anchor_stated_body_roots(&mut manifest, &inferred, &repo.path().to_string_lossy());

        assert_eq!(manifest[0].primary_type_symbol.as_deref(), Some("Member"));
        assert_eq!(manifest[0].defined_in, None);
    }

    /// Each statement here names a root the row cannot be anchored at, so
    /// the row keeps no anchor, and the fill's `Response` stays out too.
    #[test]
    fn stated_root_anchors_nothing_it_cannot_name_at_home() {
        let (repo, declaring) = stated_repo();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let outside = elsewhere.path().join("pages.ts");
        std::fs::write(&outside, "export type MemberPage = { id: string };\n").expect("write");
        // A repo can declare a type under a machinery name; it is still not
        // a body to anchor.
        std::fs::write(
            repo.path().join("src/transport.ts"),
            "export interface Response {\n  ok: boolean;\n}\n",
        )
        .expect("write");
        let transport = repo
            .path()
            .join("src/transport.ts")
            .canonicalize()
            .expect("canonical");
        let mut generic = stated_root("MemberPage", &declaring);
        generic.root_type_arguments = Some(1);
        let mut no_source = stated_root("MemberPage", &declaring);
        no_source.root_source = None;
        let cases = [
            ("written with type arguments", generic),
            ("declared nowhere the sidecar found", no_source),
            (
                "declared outside the repo",
                stated_root("MemberPage", &outside.to_string_lossy()),
            ),
            (
                "not declared under that name",
                stated_root("MembersPage", &declaring),
            ),
            (
                "transport machinery",
                stated_root("Response", &transport.to_string_lossy()),
            ),
            (
                "rooted at no name",
                crate::services::type_sidecar::StatedBody::default(),
            ),
        ];
        for (why, stated) in cases {
            let mut manifest = vec![consumer_entry("Members_Response_Call1")];
            let mut resolution = empty_resolution();
            resolution.inferred_types = vec![stated_call("Members_Response_Call1", stated)];

            anchor_stated_body_roots(
                &mut manifest,
                &resolution.inferred_types,
                &repo.path().to_string_lossy(),
            );
            enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

            assert_eq!(manifest[0].primary_type_symbol, None, "{why}");
            assert_eq!(manifest[0].defined_in, None, "{why}");
        }
    }

    /// carrick#780, case (a): the statement the v1 side writes when it was
    /// ASKED for an alias and has no shape. Composed here by the writer itself,
    /// so the test states the real coupling: whatever
    /// `append_alias_declaration` emits for a bare `unknown` must read as
    /// unresolved, not as a declaration. Unmarked, this promoted 212 entries of
    /// one indexed service to `Implicit` on the strength of a placeholder.
    #[test]
    fn enrich_reads_the_v1_placeholder_as_unresolved() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let resolution = empty_resolution();
        let mut dts = String::new();
        crate::type_manifest::append_alias_declaration(&mut dts, "OrderView", "unknown");

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, Some(&dts));

        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "v1's own placeholder must not read as a defined type: {dts}"
        );
        assert_eq!(
            aliases_to_resolve(&manifest, &HashMap::new()),
            vec!["OrderView".to_string()],
            "an alias v1 abstained on is exactly the one the capture exists to answer"
        );
    }

    /// carrick#780, case (b): a *developer-authored* `type X = unknown` reaching
    /// the bundle as the real declaration of a real API type. It carries no
    /// marker, nobody abstained, and the entry keeps the state its resolution
    /// gave it.
    #[test]
    fn enrich_does_not_read_an_authored_unknown_as_an_abstention() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let resolution = empty_resolution();
        let authored = "export type OrderView = unknown;\n";

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, Some(authored));

        assert_ne!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "a developer's own `= unknown` is not the v1 side abstaining"
        );
    }

    /// The Carrick-injected `= unknown` placeholder (carrying the marker) in the
    /// bundled .d.ts must NOT promote the entry, even if the bundle nominally
    /// "defines" the alias — it is downgraded to `Unknown` (#235).
    #[test]
    fn enrich_downgrades_trivially_unknown_dts_alias() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let resolution = empty_resolution();
        let dts = format!("export type OrderView = unknown; {MISSING_ALIAS_MARKER}\n");

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, Some(&dts));

        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
    }

    /// A *developer-authored* `type X = unknown` in a real API type carries no
    /// Carrick marker and must NOT be mistaken for the injected placeholder, so
    /// the entry keeps its state rather than being downgraded to `Unknown`
    /// (#244). The genuine `unknown` shape is still surfaced as unverifiable
    /// downstream by the check-phase top-type gates, not by a silent
    /// recall-losing downgrade here.
    #[test]
    fn enrich_does_not_downgrade_developer_authored_unknown() {
        // No resolution entry and no marker: the alias is "defined" in the
        // bundle as a genuine `= unknown`, so dts_defined_aliases promotes it to
        // Implicit and the trivially-unknown gate stays shut.
        let mut manifest = vec![consumer_entry("OrderView")];
        let resolution = empty_resolution();
        let bare = "export type OrderView = unknown;\n";

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, Some(bare));

        assert_ne!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "a developer-authored `type X = unknown` must not be downgraded to Unknown"
        );

        // And other forms a developer might write are equally not the marker.
        for form in [
            "export type OrderView = unknown;\n",
            "type OrderView = unknown;\n",
            "export type OrderView<T> = unknown;\n",
            "export declare type OrderView = unknown;\n",
            "export type OrderView = unknown; // genuinely unknown\n",
        ] {
            assert!(
                !dts_alias_is_trivially_unknown(form, "OrderView"),
                "developer-authored form must not match the placeholder marker: {form:?}"
            );
        }

        // The tagged placeholder, in the form append_missing_aliases emits, does.
        let tagged = format!("export type OrderView = unknown; {MISSING_ALIAS_MARKER}\n");
        assert!(
            dts_alias_is_trivially_unknown(&tagged, "OrderView"),
            "the Carrick-injected marker form must match the placeholder gate"
        );
    }

    /// append_missing_aliases injects a `= unknown` placeholder for a manifest
    /// alias absent from the bundle, and leaves an already-defined alias alone.
    #[test]
    fn append_missing_aliases_injects_unknown_placeholder() {
        let manifest = vec![consumer_entry("OrderView"), consumer_entry("Payment")];
        let dts = "export interface Payment { id: string }\n".to_string();

        let out = append_missing_aliases(dts, Some(&manifest));

        assert!(
            out.contains(&format!(
                "export type OrderView = unknown; {MISSING_ALIAS_MARKER}"
            )),
            "missing alias should be injected as a marked placeholder, got: {out}"
        );
        assert!(
            !out.contains("export type Payment = unknown"),
            "an already-defined alias must not be overwritten, got: {out}"
        );
        // The injected placeholder must be recognised as the Carrick placeholder.
        assert!(
            dts_alias_is_trivially_unknown(&out, "OrderView"),
            "the injected marker must be detected by the placeholder gate, got: {out}"
        );
    }

    // ---- carrick#780: what the capture's answer settles ---------------------

    fn captured(
        alias: &str,
        expanded: &str,
    ) -> crate::services::type_sidecar::ResolvedDefinitionResult {
        crate::services::type_sidecar::ResolvedDefinitionResult {
            type_alias: alias.to_string(),
            definition: expanded.to_string(),
            expanded: expanded.to_string(),
        }
    }

    /// An entry the v1 side abstained on is answered by the capture, and a real
    /// shape from there settles its state. This is the path the 2 genuinely
    /// resolved rows of one indexed service's 212 came down; marking the
    /// placeholder without it would have dropped them.
    #[test]
    fn a_real_capture_shape_settles_a_v1_abstention() {
        let mut manifest = vec![consumer_entry("OrderView")];
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

        let count = apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "{ bulkActionId: string; }")],
            &HashMap::new(),
        );

        assert_eq!(count, 1);
        assert_eq!(manifest[0].type_state, ManifestTypeState::Implicit);
        assert!(!manifest[0].is_explicit, "the capture inferred it");
        assert_eq!(manifest[0].evidence.type_state, ManifestTypeState::Implicit);
    }

    /// carrick#1516: the capture's answer for an entry's unwidened sibling is
    /// published beside the entry's own, when it says something the published
    /// type does not: never a copy of it, never one with a top type in it.
    #[test]
    fn the_unwidened_reading_is_published_beside_the_expanded_definition() {
        let sibling = crate::engine::type_compat_v2::unwidened_alias("OrderView");
        let publish = |unwidened: &str| {
            let mut manifest = vec![consumer_entry("OrderView")];
            apply_resolved_definitions(
                &mut manifest,
                vec![
                    captured("OrderView", "{ scope: string; }"),
                    captured(&sibling, unwidened),
                ],
                &HashMap::new(),
            );
            manifest.remove(0)
        };
        let entry = publish("{ scope: \"all\" | \"specific\"; }");
        assert_eq!(
            entry.expanded_definition.as_deref(),
            Some("{ scope: string; }")
        );
        assert_eq!(
            entry.unwidened_definition.as_deref(),
            Some("{ scope: \"all\" | \"specific\"; }")
        );
        assert_eq!(publish("{ scope: string; }").unwidened_definition, None);
        assert_eq!(publish("{ scope: any; }").unwidened_definition, None);

        let records = read_records_with(&sibling);
        assert_eq!(
            aliases_to_resolve(&[consumer_entry("OrderView")], &records),
            vec!["OrderView".to_string(), sibling.clone()],
            "the sibling is asked about only when the capture recorded it"
        );
    }

    /// A capture record for `alias` and nothing else.
    fn read_records_with(
        alias: &str,
    ) -> HashMap<String, crate::services::type_sidecar::CaptureAliasRecord> {
        let record: crate::services::type_sidecar::CaptureAliasRecord =
            serde_json::from_value(serde_json::json!({
                "alias": alias,
                "anchor_kind": "literal",
                "source_file": "<inline>",
                "anchor_origin": "deterministic-infer",
                "serialization": "structural_fallback",
                "self_check": "ok",
                "top_type_at_self_check": false
            }))
            .expect("a capture record");
        HashMap::from([(alias.to_string(), record)])
    }

    /// A capture answer that IS a top type describes nothing, so an entry v1
    /// abstained on publishes no definition at all rather than the word `any`.
    /// 210 of one indexed service's 212 rows are exactly this: the bundle said
    /// `unknown`, the capture said `any`, and the row read `Implicit`.
    #[test]
    fn a_shapeless_capture_answer_publishes_nothing() {
        let mut manifest = vec![consumer_entry("OrderView")];
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "any")],
            &HashMap::new(),
        );

        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
        assert_eq!(
            manifest[0].resolved_definition, None,
            "`any` is not a contract to publish; the row's provenance says why there is none"
        );
        assert_eq!(manifest[0].expanded_definition, None);
    }

    /// The same question, asked of an entry the v1 side DID answer for. Seven
    /// rows of one indexed service published `export type … = unknown;` as
    /// their resolved definition: a symbol was named, `type_state` read
    /// `Implicit`, and the boundary counted them as typed because it counts
    /// `resolved_definition.is_some()` (carrick#852).
    #[test]
    fn a_shapeless_capture_answer_publishes_nothing_for_an_answered_entry() {
        let mut manifest = vec![consumer_entry("OrderView")];
        manifest[0].type_state = ManifestTypeState::Implicit;
        manifest[0].is_explicit = false;

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "unknown")],
            &HashMap::new(),
        );

        assert_eq!(
            manifest[0].resolved_definition, None,
            "`unknown` is not a definition to publish"
        );
        assert_eq!(manifest[0].expanded_definition, None);
        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Implicit,
            "what the SOURCE states about the type is untouched: this is about \
             what the capture could resolve, not about how it was written"
        );
    }

    /// A shape with a top type inside it still describes a payload: it is
    /// published (its `any_provenance` names the decayed members, carrick#376)
    /// but it does not settle the state.
    #[test]
    fn a_partly_decayed_capture_shape_is_published_but_stays_unknown() {
        let mut manifest = vec![consumer_entry("OrderView")];
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "{ id: string; payload: any; }")],
            &HashMap::new(),
        );

        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
        assert_eq!(
            manifest[0].expanded_definition.as_deref(),
            Some("{ id: string; payload: any; }"),
            "a shape is worth publishing even when it cannot settle the state"
        );
    }

    /// carrick#1441: a v1 answer that is a real shape with a top type INSIDE it
    /// is neither a type nor an abstention. `contains_disqualifying_top_type`
    /// demotes the entry to `Unknown`, and the capture — which has the shape —
    /// used to be skipped for it, because the filter asked only about entries
    /// carrying a type or marked as a bare-top abstention. 48 entries of one
    /// real monorepo published nothing this way.
    #[test]
    fn a_v1_shape_with_a_top_type_inside_still_asks_the_capture() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        let mut inferred = inferred_with_symbol("OrderView");
        inferred.type_string = "{ relation: any; } & { id: string; }".to_string();
        inferred.primary_type_symbol = None;
        resolution.inferred_types.push(inferred);

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "a shape carrying a top type does not state a contract"
        );
        assert_eq!(
            aliases_to_resolve(&manifest, &HashMap::new()),
            vec!["OrderView".to_string()],
            "the capture must be asked: it is the layer that resolves what v1 could not"
        );

        apply_resolved_definitions(
            &mut manifest,
            vec![captured(
                "OrderView",
                "{ relation: { id: string; }; id: string; }",
            )],
            &HashMap::new(),
        );

        assert_eq!(
            manifest[0].expanded_definition.as_deref(),
            Some("{ relation: { id: string; }; id: string; }")
        );
        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Implicit,
            "a clean capture shape settles an entry no other layer stated"
        );
        assert!(!manifest[0].is_explicit, "the capture inferred it");
    }

    /// The promotion only reaches entries nothing has stated. What the SOURCE
    /// says about an entry v1 answered for is the source's to say: a capture
    /// answer publishes beside it and never restates it.
    #[test]
    fn the_capture_does_not_restate_an_entry_v1_answered_for() {
        let mut manifest = vec![consumer_entry("OrderView")];
        manifest[0].type_state = ManifestTypeState::Explicit;
        manifest[0].is_explicit = true;

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "{ id: string; }")],
            &HashMap::new(),
        );

        assert_eq!(manifest[0].type_state, ManifestTypeState::Explicit);
        assert!(manifest[0].is_explicit, "the source declared this one");
        assert_eq!(
            manifest[0].resolved_definition.as_deref(),
            Some("{ id: string; }")
        );
    }

    // ---- carrick#1752: an open member the source declares --------------------

    /// One capture finding, as the sidecar's record writes it.
    fn finding(path: &str, kind: &str, reason: &str) -> serde_json::Value {
        serde_json::json!({ "path": path, "kind": kind, "reason": reason })
    }

    /// The capture's record for `alias`, holding `findings`, already joined
    /// onto `manifest` the way the definitions pass joins it.
    fn stamp_findings(
        manifest: &mut [TypeManifestEntry],
        alias: &str,
        findings: Vec<serde_json::Value>,
    ) -> HashMap<String, crate::services::type_sidecar::CaptureAliasRecord> {
        let records = records_from(serde_json::json!([record_json(
            alias,
            serde_json::json!({
                "self_check": "decayed_internal",
                "any_provenance": findings
            }),
        )]));
        stamp_capture_provenance(manifest, &records);
        records
    }

    /// An entry v1 answered from an explicit annotation with `type_string`,
    /// after the enrichment step has read it.
    fn enriched_explicit(type_string: &str) -> Vec<TypeManifestEntry> {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        resolution
            .explicit_manifest
            .push(crate::services::type_sidecar::ManifestEntry {
                alias: "OrderView".to_string(),
                original_name: "OrderView".to_string(),
                source_file: "lib/types.ts".to_string(),
                type_string: type_string.to_string(),
                is_explicit: true,
            });
        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);
        manifest
    }

    /// carrick#1752: a client casts a response to a declared type with one
    /// member the declaration itself types `unknown`. That is a typed contract
    /// with one open field: the row keeps the state the source gave it, and
    /// the member's provenance says which field is open and why.
    #[test]
    fn an_open_member_the_source_declares_leaves_the_contract_v1_stated() {
        let shape = "{ id: string; notes: unknown; }";
        let mut manifest = enriched_explicit(shape);
        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "v1's text alone cannot say who put the `unknown` there"
        );

        let records = stamp_findings(
            &mut manifest,
            "OrderView",
            vec![finding("notes", "unknown", "declared")],
        );
        apply_resolved_definitions(&mut manifest, vec![captured("OrderView", shape)], &records);

        assert_eq!(manifest[0].expanded_definition.as_deref(), Some(shape));
        assert_eq!(manifest[0].type_state, ManifestTypeState::Explicit);
        assert!(manifest[0].is_explicit, "the source cast the call to it");
        assert_eq!(manifest[0].evidence.type_state, ManifestTypeState::Explicit);
        assert!(manifest[0].evidence.is_explicit);
        assert_eq!(
            manifest[0]
                .any_provenance
                .iter()
                .map(|p| (p.path.as_str(), p.reason.as_str()))
                .collect::<Vec<_>>(),
            vec![("notes", "declared")],
            "the open field is still named"
        );
    }

    /// The same answer for an entry no v1 layer stated settles it as the
    /// capture's own reading, the way a clean answer does.
    #[test]
    fn an_open_member_settles_an_entry_v1_did_not_answer() {
        let shape = "{ id: string; items: { sku: string; extra: unknown; }[]; }";
        let mut manifest = vec![consumer_entry("OrderView")];
        let records = stamp_findings(
            &mut manifest,
            "OrderView",
            vec![finding("items<0>.extra", "unknown", "declared")],
        );

        apply_resolved_definitions(&mut manifest, vec![captured("OrderView", shape)], &records);

        assert_eq!(manifest[0].type_state, ManifestTypeState::Implicit);
        assert!(!manifest[0].is_explicit, "the capture inferred it");
    }

    /// Everything else that puts a top type in the answer still leaves the
    /// entry `Unknown`: a pipeline artefact, an `any`, a position that is not
    /// a member of a typed shape, a list the walk may have cut short, or no
    /// record to read at all. The answer is still published in every case.
    #[test]
    fn a_top_type_the_source_did_not_declare_on_a_member_still_leaves_the_entry_unknown() {
        let open = finding("notes", "unknown", "declared");
        let capped: Vec<serde_json::Value> = (0
            ..crate::engine::type_compat_v2::CAPTURE_FINDINGS_CAP)
            .map(|i| finding(&format!("m{i}"), "unknown", "declared"))
            .collect();
        let capped_shape = format!(
            "{{ {} }}",
            (0..crate::engine::type_compat_v2::CAPTURE_FINDINGS_CAP)
                .map(|i| format!("m{i}: unknown;"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let cases: Vec<(&str, String, Vec<serde_json::Value>)> = vec![
            (
                "an unresolved import beside the declared member",
                "{ notes: unknown; row: unknown; }".to_string(),
                vec![open.clone(), finding("row", "unknown", "unresolved_import")],
            ),
            (
                "a member the source declares `any`",
                "{ id: string; notes: any; }".to_string(),
                vec![finding("notes", "any", "declared")],
            ),
            (
                "an `any` the walk does not read, in a parameter",
                "{ notes: unknown; run: (input: any) => void; }".to_string(),
                vec![open.clone()],
            ),
            (
                "a dictionary of anything at the root",
                "{ [key: string]: unknown; }".to_string(),
                vec![finding("[index]", "unknown", "declared")],
            ),
            (
                "a list of anything at the root",
                "unknown[]".to_string(),
                vec![finding("<0>", "unknown", "declared")],
            ),
            (
                "a walk that ran out of budget",
                "{ notes: unknown; }".to_string(),
                vec![
                    open.clone(),
                    finding("deep", "budget_exhausted", "budget_exhausted"),
                ],
            ),
            (
                "a text carrying `unknown` the record never reported",
                "{ notes: unknown; }".to_string(),
                vec![],
            ),
            (
                "as many findings as the walk reports before it stops listing",
                capped_shape,
                capped,
            ),
            (
                "a finding the record calls `any` where the printed text shows none",
                "{ id: string; notes: unknown; }".to_string(),
                vec![finding("notes", "any", "declared")],
            ),
        ];
        for (why, shape, findings) in cases {
            let mut manifest = enriched_explicit(&shape);
            let records = stamp_findings(&mut manifest, "OrderView", findings);
            apply_resolved_definitions(
                &mut manifest,
                vec![captured("OrderView", &shape)],
                &records,
            );
            assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown, "{why}");
            assert!(!manifest[0].is_explicit, "{why}");
            assert_eq!(
                manifest[0].expanded_definition.as_deref(),
                Some(shape.as_str()),
                "{why}: still published"
            );
        }

        let shape = "{ id: string; notes: unknown; }";
        let mut manifest = enriched_explicit(shape);
        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", shape)],
            &HashMap::new(),
        );
        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "no capture record: nothing says who declared the member"
        );

        let mut manifest = enriched_explicit(shape);
        manifest[0].any_provenance.push(
            serde_json::from_value(finding("notes", "unknown", "unresolved_import"))
                .expect("a finding"),
        );
        let records = stamp_findings(&mut manifest, "OrderView", vec![open]);
        apply_resolved_definitions(&mut manifest, vec![captured("OrderView", shape)], &records);
        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "another layer on the entry says the member did not resolve"
        );
    }

    /// A v1 answer demoted for a top type in its own text, then answered
    /// cleanly by the capture, keeps the explicitness the source gave it: the
    /// annotation is a fact about the source, not about which layer printed
    /// the shape.
    #[test]
    fn a_clean_capture_answer_keeps_the_explicit_state_v1_stated() {
        let mut manifest = enriched_explicit("{ relation: any; id: string; }");
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

        apply_resolved_definitions(
            &mut manifest,
            vec![captured(
                "OrderView",
                "{ relation: { id: string; }; id: string; }",
            )],
            &HashMap::new(),
        );

        assert_eq!(manifest[0].type_state, ManifestTypeState::Explicit);
        assert!(manifest[0].is_explicit);
    }

    /// A v1 answer that printed no shape at all states nothing about the
    /// source to return to: the symbol it named may not be the type, so the
    /// capture's clean answer reads as the capture's own.
    #[test]
    fn a_v1_answer_with_no_shape_leaves_the_capture_answer_implicit() {
        for placeholder in ["unknown", "any", ""] {
            let mut manifest = enriched_explicit(placeholder);
            assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

            apply_resolved_definitions(
                &mut manifest,
                vec![captured("OrderView", "{ id: string; }")],
                &HashMap::new(),
            );

            assert_eq!(
                manifest[0].type_state,
                ManifestTypeState::Implicit,
                "v1 answered {placeholder:?}"
            );
            assert!(!manifest[0].is_explicit, "v1 answered {placeholder:?}");
        }
    }

    // ---- carrick#1165: answers that name what does not resolve -------------

    /// A capture record as the sidecar writes it, read through the same
    /// function the scan uses, so the wire shape of the new fields is covered.
    fn records_from(
        records: serde_json::Value,
    ) -> HashMap<String, crate::services::type_sidecar::CaptureAliasRecord> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("carrick-manifest.json"),
            serde_json::json!({ "aliases": records }).to_string(),
        )
        .expect("write manifest");
        read_capture_records(dir.path())
    }

    fn record_json(alias: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut record = serde_json::json!({
            "alias": alias,
            "anchor_kind": "infer",
            "source_file": "lib/api.ts",
            "anchor_origin": "deterministic-infer",
            "serialization": "node_builder",
            "self_check": "ok",
            "top_type_at_self_check": false
        });
        for (key, value) in extra.as_object().expect("object").iter() {
            record[key] = value.clone();
        }
        record
    }

    fn answered_entry() -> TypeManifestEntry {
        let mut entry = consumer_entry("OrderView");
        entry.type_state = ManifestTypeState::Implicit;
        entry
    }

    // ---- carrick#1446: positions that do not resolve in the emitted tree ---

    fn unresolved(path: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "kind": "any",
            "reason": "unresolved_import",
            "detail": "does not resolve in the declarations the capture emitted"
        })
    }

    fn provenance_of(entry: &TypeManifestEntry) -> Vec<(&str, &str)> {
        entry
            .any_provenance
            .iter()
            .map(|p| (p.path.as_str(), p.reason.as_str()))
            .collect()
    }

    /// A member the emitted tree cannot resolve prints as the name it could
    /// not follow, so the published shape reads typed. The record's list of
    /// such positions reaches the entry a reader sees, beside the entry's
    /// other findings, and the shape is still published.
    #[test]
    fn positions_the_emitted_tree_cannot_resolve_reach_the_entry() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({
                "any_provenance": [finding("meta", "any", "declared")],
                "unresolved_in_tree": [unresolved("items<0>.status")]
            }),
        )]));
        let mut manifest = vec![answered_entry(), consumer_entry("Untouched")];
        let shape = "{ meta: any; items: { status: OrderStatus; }[]; }";

        join_capture_answers(&mut manifest, vec![captured("OrderView", shape)], &records);

        assert_eq!(manifest[0].expanded_definition.as_deref(), Some(shape));
        assert_eq!(
            provenance_of(&manifest[0]),
            vec![
                ("items<0>.status", "unresolved_import"),
                ("meta", "declared")
            ]
        );
        assert!(manifest[1].any_provenance.is_empty());
    }

    /// An entry served from v1's text, because the capture's answer did not
    /// resolve at its root, says so: the text is a literal anchor's, and it
    /// names what nothing declares (carrick#1165's fallback rows).
    #[test]
    fn an_answer_that_does_not_resolve_at_its_root_says_so_on_the_entry() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({
                "anchor_kind": "literal",
                "self_check": "decayed_internal",
                "top_type_at_self_check": true,
                "unresolved_in_tree": [unresolved("")]
            }),
        )]));
        let mut manifest = vec![answered_entry()];

        join_capture_answers(&mut manifest, vec![captured("OrderView", "any")], &records);

        assert_eq!(manifest[0].resolved_definition, None);
        assert_eq!(manifest[0].type_state, ManifestTypeState::Implicit);
        assert_eq!(provenance_of(&manifest[0]), vec![("", "unresolved_import")]);
    }

    /// The list joins AFTER the answer settles the state. An entry no v1 layer
    /// stated, whose answer has a member the source declares `unknown` and a
    /// member the tree cannot resolve, settles like the same answer without
    /// the declared member does: the unresolved name is not a top type in the
    /// text, so it has nothing for the declared-open-member rule to explain.
    #[test]
    fn an_unresolved_member_does_not_stop_an_open_contract_settling() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({
                "any_provenance": [finding("notes", "unknown", "declared")],
                "unresolved_in_tree": [unresolved("status")]
            }),
        )]));
        let mut manifest = vec![consumer_entry("OrderView")];
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);

        join_capture_answers(
            &mut manifest,
            vec![captured(
                "OrderView",
                "{ id: string; notes: unknown; status: OrderStatus; }",
            )],
            &records,
        );

        assert_eq!(manifest[0].type_state, ManifestTypeState::Implicit);
        assert_eq!(
            provenance_of(&manifest[0]),
            vec![("notes", "declared"), ("status", "unresolved_import")]
        );
    }

    /// A shape that names an identifier nothing declares reads as a type and
    /// is not one: the reader cannot resolve `ParcelRow`, and the boundary
    /// counted the operation typed.
    #[test]
    fn an_answer_naming_an_undeclared_identifier_is_not_published() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({ "undeclared_names": ["ParcelRow"] }),
        )]));
        let mut manifest = vec![answered_entry()];

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "{ id: string; parcel: ParcelRow; }")],
            &records,
        );

        assert_eq!(manifest[0].resolved_definition, None);
        assert_eq!(manifest[0].expanded_definition, None);
    }

    /// An answer whose declaration imports a module the checkout does not
    /// have prints the missing module's names (`Shape<Options>`, a builder's
    /// field reference over `any`) with nothing behind them.
    #[test]
    fn an_answer_whose_declaration_imports_a_missing_module_is_not_published() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({
                "self_check": "decayed_internal",
                "dangling_specifiers": ["./generated/client"]
            }),
        )]));
        let mut manifest = vec![answered_entry()];

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "Shape<Options>")],
            &records,
        );

        assert_eq!(manifest[0].resolved_definition, None);
    }

    /// A top type with nothing to heal it is not published under a name; one a
    /// pinned external explains is, because the check phase installs the pin
    /// and the name then means something.
    #[test]
    fn a_top_type_answer_is_published_only_when_a_pinned_external_explains_it() {
        let records = records_from(serde_json::json!([
            record_json(
                "OrderView",
                serde_json::json!({
                    "self_check": "decayed_internal",
                    "top_type_at_self_check": true
                }),
            ),
            record_json(
                "Healable",
                serde_json::json!({
                    "self_check": "allowlisted_external",
                    "top_type_at_self_check": true
                }),
            ),
        ]));
        let mut healable = answered_entry();
        healable.type_alias = "Healable".to_string();
        let mut manifest = vec![answered_entry(), healable];

        apply_resolved_definitions(
            &mut manifest,
            vec![
                captured("OrderView", "ParcelRow"),
                captured("Healable", "import(\"kit\").Context"),
            ],
            &records,
        );

        assert_eq!(manifest[0].resolved_definition, None);
        assert_eq!(
            manifest[1].expanded_definition.as_deref(),
            Some("import(\"kit\").Context")
        );
    }

    /// A record without the new fields (a stub written before them) publishes
    /// exactly as before.
    #[test]
    fn a_record_without_the_new_fields_publishes_as_before() {
        let records = records_from(serde_json::json!([record_json(
            "OrderView",
            serde_json::json!({}),
        )]));
        let mut manifest = vec![answered_entry()];

        apply_resolved_definitions(
            &mut manifest,
            vec![captured("OrderView", "{ id: string; }")],
            &records,
        );

        assert_eq!(
            manifest[0].expanded_definition.as_deref(),
            Some("{ id: string; }")
        );
    }

    /// A v1 answer that is a bare `any` describes nothing, the same abstention
    /// as the marked placeholder. The capture is asked, and its shape is
    /// published; with `any` members inside, it does not settle the state.
    #[test]
    fn a_bare_any_v1_answer_asks_the_capture() {
        let mut manifest = vec![consumer_entry("OrderView")];
        let mut resolution = empty_resolution();
        let mut inferred = inferred_with_symbol("OrderView");
        inferred.type_string = "any".to_string();
        inferred.primary_type_symbol = None;
        resolution.inferred_types.push(inferred);

        enrich_manifest_with_type_resolution(&mut manifest, &resolution, None);

        assert_eq!(
            manifest[0].type_state,
            ManifestTypeState::Unknown,
            "a bare `any` is an abstention"
        );
        assert_eq!(
            aliases_to_resolve(&manifest, &HashMap::new()),
            vec!["OrderView".to_string()]
        );

        apply_resolved_definitions(
            &mut manifest,
            vec![captured(
                "OrderView",
                "{ id: any; status: \"open\" | \"done\"; }",
            )],
            &HashMap::new(),
        );

        assert_eq!(
            manifest[0].expanded_definition.as_deref(),
            Some("{ id: any; status: \"open\" | \"done\"; }")
        );
        assert_eq!(manifest[0].type_state, ManifestTypeState::Unknown);
    }

    // ---- #245 Phase 1: protocol op manifest entries -------------------------

    fn socket_op(
        event: &str,
        direction: crate::operation::SocketDirection,
        symbol: Option<&str>,
        source: Option<&str>,
    ) -> crate::socket_io::SocketOp {
        crate::socket_io::SocketOp {
            key: OperationKey::socket(event, direction),
            file_path: PathBuf::from("src/socket.ts"),
            line: 12,
            payload_type_symbol: symbol.map(String::from),
            payload_type_source: source.map(String::from),
        }
    }

    fn graphql_op(
        kind: crate::operation::GraphqlOperationKind,
        field: &str,
        anchor: Option<&str>,
    ) -> crate::graphql::GraphqlOp {
        crate::graphql::GraphqlOp {
            key: OperationKey::graphql(kind, field),
            file_path: PathBuf::from("src/schema.graphql"),
            line: 3,
            document_line: 1,
            primary_type_symbol: anchor.map(String::from),
            payload_type_symbol: None,
            payload_type_source: None,
            resolver_file: None,
            resolver_line: None,
            response_type_symbol: None,
            response_type_source: None,
            consumer_located_type_symbol: None,
            consumer_located_type_source: None,
            declared_result_type: None,
            operation: None,
            located_field_type: None,
            schema_binding: None,
            arguments: None,
        }
    }

    /// A GraphQL consumer op carrying a `request<T>` call-site anchor in
    /// `payload_type_symbol` (the field SDL producers can't provide).
    fn graphql_consumer_op(
        field: &str,
        payload_symbol: Option<&str>,
        payload_source: Option<&str>,
    ) -> crate::graphql::GraphqlOp {
        crate::graphql::GraphqlOp {
            key: OperationKey::graphql(crate::operation::GraphqlOperationKind::Query, field),
            file_path: PathBuf::from("web-frontend/lib/graphql.ts"),
            line: 76,
            document_line: 70,
            primary_type_symbol: None,
            payload_type_symbol: payload_symbol.map(String::from),
            payload_type_source: payload_source.map(String::from),
            resolver_file: None,
            resolver_line: None,
            response_type_symbol: None,
            response_type_source: None,
            consumer_located_type_symbol: None,
            consumer_located_type_source: None,
            declared_result_type: None,
            operation: None,
            located_field_type: None,
            schema_binding: None,
            arguments: None,
        }
    }

    /// #307 (class 2) helper: an LLM HTTP data call at a given file/target.
    fn transport_call(target: &str, file_location: &str) -> crate::mount_graph::DataFetchingCall {
        crate::mount_graph::DataFetchingCall {
            method: "POST".to_string(),
            target_url: target.to_string(),
            canonical_path: target.to_string(),
            client: "fetch(".to_string(),
            file_location: file_location.to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
            role: None,
            reaches_request: None,
            library_semantics: Vec::new(),
        }
    }

    /// #307 (class 2): an env-templated HTTP call in a file whose gql documents
    /// produced consumer ops is that file's transport — folded. A relative
    /// literal call in the same file (same-origin REST) and an env-templated
    /// call in an unrelated file both stay.
    #[test]
    fn fold_drops_graphql_transport_calls_only() {
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![
            transport_call("${SUPPORT_GQL_URL}/graphql", "src/gql.ts:25"),
            transport_call("/api/tickets", "src/gql.ts:30"),
            transport_call("${ORDERS_API}/orders", "src/orders.ts:12"),
        ];
        let graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![graphql_consumer_op_at(
                crate::operation::GraphqlOperationKind::Mutation,
                "escalateTicket",
                "src/gql.ts",
                None,
            )],
            input_declarations: Default::default(),
        };

        fold_graphql_transport_calls(&mut mount_graph, &graphql);

        let targets: Vec<&str> = mount_graph
            .data_calls
            .iter()
            .map(|c| c.target_url.as_str())
            .collect();
        assert_eq!(targets, vec!["/api/tickets", "${ORDERS_API}/orders"]);
    }

    /// A declared-internal env-var base strips to a bare path in
    /// `canonical_path` (`${GQL_URL}/graphql` → `/graphql`), so the transport
    /// shape must be read off the RAW target or the fold would leak for
    /// exactly the users who configured `internalEnvVars` (Copilot review).
    #[test]
    fn fold_reads_raw_target_not_stripped_canonical() {
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![crate::mount_graph::DataFetchingCall {
            method: "POST".to_string(),
            target_url: "`${SUPPORT_GQL_URL}/graphql`".to_string(),
            canonical_path: "/graphql".to_string(),
            client: "fetch(".to_string(),
            file_location: "src/gql.ts:25".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
            role: None,
            reaches_request: None,
            library_semantics: Vec::new(),
        }];
        let graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![graphql_consumer_op_at(
                crate::operation::GraphqlOperationKind::Mutation,
                "escalateTicket",
                "src/gql.ts",
                None,
            )],
            input_declarations: Default::default(),
        };

        fold_graphql_transport_calls(&mut mount_graph, &graphql);

        assert!(
            mount_graph.data_calls.is_empty(),
            "internal-stripped canonical must still fold via the raw target"
        );
    }

    /// The file join must normalize path components: the graphql walk can
    /// yield a `./`-prefixed path while the data call's file key is bare.
    #[test]
    fn fold_joins_dot_prefixed_walk_paths() {
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![transport_call(
            "https://support.example.com/graphql",
            "src/gql.ts:25",
        )];
        let graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![graphql_consumer_op_at(
                crate::operation::GraphqlOperationKind::Query,
                "ticket",
                "./src/gql.ts",
                None,
            )],
            input_declarations: Default::default(),
        };

        fold_graphql_transport_calls(&mut mount_graph, &graphql);

        assert!(
            mount_graph.data_calls.is_empty(),
            "absolute-URL transport in a ./-walked gql-consumer file must fold"
        );
    }

    /// Producer-only extractions (an SDL service with no documents) must not
    /// fold anything — the transport fold is a CONSUMER-side dedup.
    #[test]
    fn fold_ignores_producer_only_extractions() {
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![transport_call("${SOME_API}/things", "src/schema.ts:5")];
        let graphql = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                crate::operation::GraphqlOperationKind::Query,
                "order",
                Some("Order"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        fold_graphql_transport_calls(&mut mount_graph, &graphql);

        assert_eq!(mount_graph.data_calls.len(), 1);
    }

    /// carrick#1134: a document written against a schema no service serves is
    /// not a call, and its file's transport call is folded all the same, so
    /// the vendor POST does not come back as an HTTP call once the document is
    /// gone.
    #[test]
    fn settle_drops_external_documents_and_still_folds_their_transport() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("vendor")).unwrap();
        std::fs::write(
            repo.path().join("vendor/schema.graphql"),
            "type Query { balance: Int }",
        )
        .unwrap();
        let catalogue = crate::graphql::SchemaCatalogue::build(
            repo.path(),
            &[crate::graphql::ServedSchemaSources {
                roots: vec![repo.path().join("src")],
                declared: vec![],
            }],
        );
        let mut mount_graph = MountGraph::new();
        mount_graph.data_calls = vec![
            transport_call("${LEDGER_URL}/graphql", "src/gql.ts:25"),
            transport_call("${ORDERS_API}/orders", "src/orders.ts:12"),
        ];
        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![graphql_consumer_op_at(
                crate::operation::GraphqlOperationKind::Query,
                "balance",
                "src/gql.ts",
                None,
            )],
            input_declarations: Default::default(),
        };

        settle_graphql_documents(
            &mut graphql,
            Default::default(),
            &HashMap::new(),
            "",
            &modules_without_config(),
            &mut mount_graph,
            &Config::default(),
            &catalogue,
        );

        assert!(
            graphql.consumers.is_empty(),
            "the external document is not a call"
        );
        let targets: Vec<&str> = mount_graph
            .data_calls
            .iter()
            .map(|c| c.target_url.as_str())
            .collect();
        assert_eq!(targets, vec!["${ORDERS_API}/orders"]);
        assert_eq!(catalogue.notices().len(), 1);
    }

    /// Variant of `graphql_consumer_op` with a caller-chosen `kind` and
    /// `file_path`, for the #268 per-file/per-kind join tests: the consumer
    /// locate merge is keyed on `(file_path, kind, field)`, so exercising
    /// fan-in across files or a non-Query kind needs a helper that can vary
    /// both (the plain `graphql_consumer_op` fixes kind=Query and
    /// file_path="web-frontend/lib/graphql.ts").
    fn graphql_consumer_op_at(
        kind: crate::operation::GraphqlOperationKind,
        field: &str,
        file_path: &str,
        payload_symbol: Option<&str>,
    ) -> crate::graphql::GraphqlOp {
        crate::graphql::GraphqlOp {
            key: OperationKey::graphql(kind, field),
            file_path: PathBuf::from(file_path),
            line: 10,
            document_line: 9,
            primary_type_symbol: None,
            payload_type_symbol: payload_symbol.map(String::from),
            payload_type_source: None,
            resolver_file: None,
            resolver_line: None,
            response_type_symbol: None,
            response_type_source: None,
            consumer_located_type_symbol: None,
            consumer_located_type_source: None,
            declared_result_type: None,
            operation: None,
            located_field_type: None,
            schema_binding: None,
            arguments: None,
        }
    }

    /// A pub/sub op the file-analyzer would emit: topic + side + decoded-payload
    /// `primary_type_symbol`. Mirrors `socket_op`/`graphql_op` for the manifest
    /// tests.
    /// One library row for the engine tests (carrick#1662).
    fn library_row(
        file: &str,
        line: u32,
        name: &str,
        kind: crate::library_claims::LibraryRowKind,
    ) -> crate::library_claims::LibraryRow {
        crate::library_claims::LibraryRow {
            file: PathBuf::from(file),
            line,
            span_start: 10,
            span_end: 40,
            kind,
            name: name.to_string(),
            name_scope: crate::services::type_sidecar::NameScope {
                scope: crate::services::type_sidecar::NameScopeKind::Service,
                namespace: Some("task".to_string()),
            },
            claim_ids: vec![format!("@fixture/jobs@3:task:op:{name}")],
            definition: false,
        }
    }

    /// carrick#1662: a library row is a `library_claim` fact carrying its
    /// name's scope and its claim ids, on the producer side for a subscriber
    /// or a listener and the call side for a publisher or an emitter. A row
    /// in a mock tree of the service states nothing. A model pub/sub row at
    /// the same file, line, topic and role folds into the library row, in
    /// the operations and in the type manifest; one at another line stands.
    #[test]
    fn library_rows_are_stated_as_facts_and_fold_the_model_s_row_at_their_site() {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::library_claims::{LibraryRowKind, LibraryRows};
        use crate::operation::{PubsubRole, SocketDirection};

        let extractions = ProtocolExtractions {
            library: LibraryRows {
                rows: vec![
                    library_row(
                        "svc/src/tasks.ts",
                        2,
                        "send-email",
                        LibraryRowKind::Pubsub(PubsubRole::Subscriber),
                    ),
                    library_row(
                        "svc/src/orders.ts",
                        14,
                        "orders.created",
                        LibraryRowKind::Pubsub(PubsubRole::Publisher),
                    ),
                    library_row(
                        "svc/src/live.ts",
                        4,
                        "chat",
                        LibraryRowKind::Socket {
                            direction: SocketDirection::ClientToServer,
                            listener: true,
                        },
                    ),
                    library_row(
                        "svc/src/live.ts",
                        5,
                        "typing",
                        LibraryRowKind::Socket {
                            direction: SocketDirection::ServerToClient,
                            listener: false,
                        },
                    ),
                    library_row(
                        "svc/src/__mocks__/tasks.ts",
                        3,
                        "mocked",
                        LibraryRowKind::Pubsub(PubsubRole::Subscriber),
                    ),
                ],
            },
            ..ProtocolExtractions::default()
        };
        let mut elsewhere = pubsub_op("orders.created", PubsubRole::Publisher, None, None);
        elsewhere.line_number = 20;
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "svc/src/orders.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![
                    pubsub_op("orders.created", PubsubRole::Publisher, None, None),
                    elsewhere,
                ],
                ..Default::default()
            },
        );
        let mut cloud_data = repo_with_bundle("svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &file_results,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            ".",
            &Config::default(),
        );

        let task = cloud_data
            .endpoints
            .iter()
            .find(|row| row.key.canonical() == "pubsub|send-email")
            .expect("the subscriber is a producer");
        assert_eq!(task.resolution_source, Some(ResolutionSource::LibraryClaim));
        assert_eq!(
            task.name_scope
                .as_ref()
                .map(|scope| scope.namespace.clone()),
            Some(Some("task".to_string()))
        );
        assert_eq!(
            task.library_semantics,
            vec!["@fixture/jobs@3:task:op:send-email"]
        );
        assert_eq!(task.file_path, PathBuf::from("svc/src/tasks.ts:2"));
        let published: Vec<(&PathBuf, Option<ResolutionSource>)> = cloud_data
            .calls
            .iter()
            .filter(|row| row.key.canonical() == "pubsub|orders.created")
            .map(|row| (&row.file_path, row.resolution_source))
            .collect();
        assert_eq!(
            published,
            vec![
                (
                    &PathBuf::from("svc/src/orders.ts:14"),
                    Some(ResolutionSource::LibraryClaim)
                ),
                (
                    &PathBuf::from("svc/src/orders.ts:20"),
                    Some(ResolutionSource::Model)
                ),
            ],
            "the model's row at the library row's site folds; the other stands"
        );
        assert!(cloud_data.endpoints.iter().any(|row| row.key
            == OperationKey::socket("chat", SocketDirection::ClientToServer)
            && row.resolution_source == Some(ResolutionSource::LibraryClaim)));
        assert!(cloud_data.calls.iter().any(|row| row.key
            == OperationKey::socket("typing", SocketDirection::ServerToClient)
            && row.resolution_source == Some(ResolutionSource::LibraryClaim)));
        assert!(
            !cloud_data
                .endpoints
                .iter()
                .any(|row| row.key.canonical() == "pubsub|mocked"),
            "a mock tree states nothing"
        );

        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::of(
                &stated_library_rows(&extractions.library, ".", &Config::default()),
                ".",
            ),
            ".",
        );
        assert_eq!(
            entries.len(),
            1,
            "only the model row that stands is anchored"
        );
    }

    /// carrick#1662: an event-bus row at a library row's file, line and name
    /// is the same call read again, and is left out; one elsewhere stands.
    #[test]
    fn an_event_bus_row_at_a_library_row_s_site_is_left_out() {
        use crate::library_claims::{LibraryRowKind, LibraryRows};
        use crate::operation::PubsubRole;

        let bus_op = |line: u32| crate::event_emitter::BusOp {
            key: OperationKey::pubsub("ready"),
            event: "ready".to_string(),
            file_path: PathBuf::from("svc/src/events.ts"),
            line,
        };
        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction {
                subscribers: vec![bus_op(4), bus_op(9)],
                publishers: Vec::new(),
            },
            library: LibraryRows {
                rows: vec![library_row(
                    "svc/src/events.ts",
                    4,
                    "ready",
                    LibraryRowKind::Pubsub(PubsubRole::Subscriber),
                )],
            },
            ..ProtocolExtractions::default()
        };
        let mut cloud_data = repo_with_bundle("svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &HashMap::new(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            ".",
            &Config::default(),
        );
        let lines: Vec<(
            &PathBuf,
            Option<crate::agents::file_analyzer_agent::ResolutionSource>,
        )> = cloud_data
            .endpoints
            .iter()
            .filter(|row| row.key.canonical() == "pubsub|ready")
            .map(|row| (&row.file_path, row.resolution_source))
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    &PathBuf::from("svc/src/events.ts:4"),
                    Some(crate::agents::file_analyzer_agent::ResolutionSource::LibraryClaim)
                ),
                (&PathBuf::from("svc/src/events.ts:9"), None),
            ]
        );
    }

    /// carrick#1662: a model route at exactly a verified definition's span is
    /// withdrawn. It fails closed: a model route at another span, a route a
    /// pass stated, and a row that is no definition keep their routes.
    #[test]
    fn a_model_route_at_a_definition_s_span_is_withdrawn_and_nothing_else() {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::library_claims::{LibraryRowKind, LibraryRows};
        use crate::operation::PubsubRole;

        let endpoint = |line: i32, path: &str, span: (u32, u32)| EndpointResult {
            line_number: line,
            method: "POST".to_string(),
            path: path.to_string(),
            call_expression_span_start: Some(span.0),
            call_expression_span_end: Some(span.1),
            ..endpoint_with_handler("run")
        };
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "svc/src/tasks.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![
                    endpoint(2, "/send-email", (10, 40)),
                    endpoint(5, "/awaited", (60, 98)),
                    endpoint(8, "/stated", (120, 150)),
                    endpoint(11, "/not-a-definition", (170, 200)),
                ],
                ..Default::default()
            },
        );
        let route = |line: u32, path: &str, source| crate::mount_graph::ResolvedEndpoint {
            view_module: false,
            method: "POST".to_string(),
            path: path.to_string(),
            full_path: path.to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: format!("svc/src/tasks.ts:{line}"),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: Some(source),
            dispatch: None,
            handler_span: None,
        };
        let mut mount_graph = crate::mount_graph::MountGraph::new();
        mount_graph.endpoints = vec![
            route(2, "/send-email", ResolutionSource::Model),
            route(5, "/awaited", ResolutionSource::Model),
            route(8, "/stated", ResolutionSource::FileBasedRoute),
            route(11, "/not-a-definition", ResolutionSource::Model),
        ];
        let definition = |line: u32, name: &str, span: (u32, u32), definition: bool| {
            crate::library_claims::LibraryRow {
                span_start: span.0,
                span_end: span.1,
                definition,
                ..library_row(
                    "svc/src/tasks.ts",
                    line,
                    name,
                    LibraryRowKind::Pubsub(PubsubRole::Subscriber),
                )
            }
        };
        let library = LibraryRows {
            rows: vec![
                definition(2, "send-email", (10, 40), true),
                // `await task(…)`: the model's span holds the `await`.
                definition(5, "awaited", (66, 98), true),
                definition(8, "stated", (120, 150), true),
                definition(11, "not-a-definition", (170, 200), false),
            ],
        };
        let withdrawn =
            withdraw_model_routes_at_definitions(&mut mount_graph, &file_results, &library, ".");
        assert_eq!(withdrawn, 1);
        let kept: Vec<&str> = mount_graph
            .endpoints
            .iter()
            .map(|route| route.path.as_str())
            .collect();
        assert_eq!(kept, vec!["/awaited", "/stated", "/not-a-definition"]);

        // The full scan's analysis keys, graph and library rows hold the
        // walked path, absolute when the repo path is.
        let mut absolute_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        absolute_results.insert(
            "/repo/svc/src/tasks.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![endpoint(2, "/send-email", (10, 40))],
                ..Default::default()
            },
        );
        let mut absolute_graph = crate::mount_graph::MountGraph::new();
        absolute_graph.endpoints = vec![crate::mount_graph::ResolvedEndpoint {
            file_location: "/repo/svc/src/tasks.ts:2".to_string(),
            ..route(2, "/send-email", ResolutionSource::Model)
        }];
        let absolute_library = LibraryRows {
            rows: vec![crate::library_claims::LibraryRow {
                file: PathBuf::from("/repo/svc/src/tasks.ts"),
                ..definition(2, "send-email", (10, 40), true)
            }],
        };
        assert_eq!(
            withdraw_model_routes_at_definitions(
                &mut absolute_graph,
                &absolute_results,
                &absolute_library,
                "/repo",
            ),
            1,
            "an absolute repo path withdraws the same route"
        );
    }

    /// A socket pass op at `file:line`, keyed `name` in `direction`.
    fn socket_op_at(
        file: &str,
        line: u32,
        name: &str,
        direction: crate::operation::SocketDirection,
    ) -> crate::socket_io::SocketOp {
        crate::socket_io::SocketOp {
            key: OperationKey::socket(name, direction),
            file_path: PathBuf::from(file),
            line,
            payload_type_symbol: None,
            payload_type_source: None,
        }
    }

    /// Ruled on carrick#1664: a socket pass row folds into the verified
    /// library socket row at the same file, line, name, direction and side,
    /// as an event-bus row does; a row whose direction the pass could not
    /// read folds into one at the same file, line, name and side, whose
    /// direction wins. Any other socket row stands, the opposite side at the
    /// same site included, and with no library row the pass's row stands.
    /// The repo path is absolute and the pass's files are too, as a full scan
    /// walks them; the library rows are relative.
    #[test]
    fn a_socket_pass_row_folds_into_the_library_row_at_its_site_and_no_other() {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::library_claims::{LibraryRowKind, LibraryRows};
        use crate::operation::SocketDirection::{ClientToServer, ServerToClient, Unknown};

        let live = "/repo/svc/src/live.ts";
        let extractions = ProtocolExtractions {
            library: LibraryRows {
                rows: vec![
                    library_row(
                        "svc/src/live.ts",
                        4,
                        "chat",
                        LibraryRowKind::Socket {
                            direction: ClientToServer,
                            listener: true,
                        },
                    ),
                    library_row(
                        "svc/src/live.ts",
                        5,
                        "typing",
                        LibraryRowKind::Socket {
                            direction: ServerToClient,
                            listener: false,
                        },
                    ),
                ],
            },
            sockets: crate::socket_io::SocketExtraction {
                listeners: vec![
                    socket_op_at(live, 4, "chat", ClientToServer),
                    socket_op_at(live, 4, "chat", Unknown),
                    socket_op_at(live, 4, "chat", ServerToClient),
                    socket_op_at(live, 9, "chat", ClientToServer),
                    socket_op_at("/repo/svc/src/other.ts", 4, "chat", ClientToServer),
                    socket_op_at(live, 4, "chat-room", ClientToServer),
                    socket_op_at(live, 5, "typing", ServerToClient),
                    socket_op_at(live, 5, "typing", Unknown),
                ],
                emitters: vec![
                    socket_op_at(live, 5, "typing", ServerToClient),
                    socket_op_at(live, 5, "typing", Unknown),
                    socket_op_at(live, 4, "chat", ClientToServer),
                    socket_op_at(live, 4, "chat", Unknown),
                ],
            },
            ..ProtocolExtractions::default()
        };
        let mut cloud_data = repo_with_bundle("svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &HashMap::new(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            "/repo",
            &Config::default(),
        );
        let rows =
            |rows: &[ApiEndpointDetails]| -> Vec<(String, String, Option<ResolutionSource>)> {
                let mut rows: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        (
                            row.key.canonical(),
                            row.file_path.display().to_string(),
                            row.resolution_source,
                        )
                    })
                    .collect();
                rows.sort();
                rows
            };
        let fact = Some(ResolutionSource::LibraryClaim);
        let row = |key: &str, at: &str, source: Option<ResolutionSource>| {
            (key.to_string(), at.to_string(), source)
        };
        let (live4, live5) = ("/repo/svc/src/live.ts:4", "/repo/svc/src/live.ts:5");
        assert_eq!(
            rows(&cloud_data.endpoints),
            vec![
                row(
                    "socket|CLIENT->SERVER|chat",
                    "/repo/svc/src/live.ts:9",
                    None
                ),
                row(
                    "socket|CLIENT->SERVER|chat",
                    "/repo/svc/src/other.ts:4",
                    None
                ),
                row("socket|CLIENT->SERVER|chat", "svc/src/live.ts:4", fact),
                row("socket|CLIENT->SERVER|chat-room", live4, None),
                row("socket|SERVER->CLIENT|chat", live4, None),
                row("socket|SERVER->CLIENT|typing", live5, None),
                row("socket|UNKNOWN|typing", live5, None),
            ],
            "the listeners at the library listener's site in its direction or none fold; \
             every other listener stands, those at the library emitter's site included"
        );
        assert_eq!(
            rows(&cloud_data.calls),
            vec![
                row("socket|CLIENT->SERVER|chat", live4, None),
                row("socket|SERVER->CLIENT|typing", "svc/src/live.ts:5", fact),
                row("socket|UNKNOWN|chat", live4, None),
            ],
            "the emitters at the library emitter's site fold; those at a listener's site stand"
        );

        // An op folded in its own direction keeps its payload anchor, which
        // types the library row on the same key; one of unknown direction
        // loses it, as no operation has its key any more.
        let mut entries = Vec::new();
        append_protocol_manifest_entries(
            &mut entries,
            &extractions,
            &LibrarySiteIndex::of(
                &stated_library_rows(&extractions.library, "/repo", &Config::default()),
                "/repo",
            ),
        );
        let anchored: Vec<(String, bool, u32)> = {
            let mut anchored: Vec<(String, bool, u32)> = entries
                .iter()
                .filter(|entry| entry.file_path == live && entry.line_number <= 5)
                .map(|entry| {
                    (
                        entry.key.canonical(),
                        entry.role == ManifestRole::Producer,
                        entry.line_number,
                    )
                })
                .collect();
            anchored.sort();
            anchored
        };
        let anchor = |key: &str, listener: bool, line: u32| (key.to_string(), listener, line);
        assert_eq!(
            anchored,
            vec![
                anchor("socket|CLIENT->SERVER|chat", false, 4),
                anchor("socket|CLIENT->SERVER|chat", true, 4),
                anchor("socket|CLIENT->SERVER|chat-room", true, 4),
                anchor("socket|SERVER->CLIENT|chat", true, 4),
                anchor("socket|SERVER->CLIENT|typing", false, 5),
                anchor("socket|SERVER->CLIENT|typing", true, 5),
                anchor("socket|UNKNOWN|chat", false, 4),
                anchor("socket|UNKNOWN|typing", true, 5),
            ],
            "the folded listener of unknown direction at line 4 and emitter at line 5 \
             keep no anchor; every other op at those lines does"
        );
    }

    /// carrick#1662: with no claims, or no sidecar to verify them, nothing
    /// is read.
    #[test]
    fn no_claims_or_no_sidecar_read_no_library_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("index.ts"),
            "import { bus } from \"@fixture/bus\";\nbus.publish(\"ready\", {});\n",
        )
        .expect("write");
        let sites = crate::request_summary::library_sites(&discover_request_inputs(dir.path()));
        assert!(
            read_library_rows(&sites, &[], None, dir.path())
                .rows
                .is_empty()
        );
        let claims: Vec<crate::library_claims::ExportClaims> = vec![
            serde_json::from_value(serde_json::json!({
                "package": "@fixture/bus", "version": "1.0.0", "specifier": "@fixture/bus",
                "export": "bus", "role": "broker",
                "claims": [{ "kind": "op", "op": "send", "member": "publish", "on": "export",
                             "name": { "arg": 0 }, "payload": { "arg": 1 } }]
            }))
            .expect("claims"),
        ];
        assert!(
            read_library_rows(&sites, &claims, None, dir.path())
                .rows
                .is_empty()
        );
    }

    fn pubsub_op(
        topic: &str,
        role: crate::operation::PubsubRole,
        symbol: Option<&str>,
        source: Option<&str>,
    ) -> crate::agents::file_analyzer_agent::PubsubOperation {
        crate::agents::file_analyzer_agent::PubsubOperation {
            topic: topic.to_string(),
            role: Some(role),
            line_number: 14,
            primary_type_symbol: symbol.map(String::from),
            type_import_source: source.map(String::from),
            broker: Some("redis".to_string()),
            payload_expression_text: None,
            payload_expression_line: None,
            backfilled: false,
        }
    }

    /// carrick#1626, on the wire: a pub/sub row the model stated is uploaded
    /// with `resolution_source: "model"`, and one the scanner backfilled into
    /// the model's list states no source, so the key is absent.
    #[test]
    fn model_pubsub_rows_are_stamped_model_and_backfilled_rows_state_nothing() {
        use crate::operation::PubsubRole;

        let mut backfilled = pubsub_op("orders.cancelled", PubsubRole::Subscriber, None, None);
        backfilled.backfilled = true;
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "svc/src/orders.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![
                    pubsub_op("orders.created", PubsubRole::Publisher, None, None),
                    pubsub_op("orders.shipped", PubsubRole::Subscriber, None, None),
                    backfilled,
                ],
                ..Default::default()
            },
        );
        let mut cloud_data = repo_with_bundle("svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &ProtocolExtractions::default(),
            &file_results,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            ".",
            &Config::default(),
        );

        let wire = serde_json::to_value(&cloud_data).expect("the repo data serializes");
        let row = |side: &str, topic: &str| -> serde_json::Value {
            wire[side]
                .as_array()
                .expect("an operations array")
                .iter()
                .find(|row| row["key"]["topic"] == topic)
                .cloned()
                .unwrap_or_else(|| panic!("no {side} row for {topic}: {wire}"))
        };
        assert_eq!(row("calls", "orders.created")["resolution_source"], "model");
        assert_eq!(
            row("endpoints", "orders.shipped")["resolution_source"],
            "model"
        );
        let scanner_row = row("endpoints", "orders.cancelled");
        assert!(
            scanner_row.get("resolution_source").is_none(),
            "a backfilled row is not the model's: {scanner_row}"
        );
    }

    /// #380 for every protocol (carrick#1626): a GraphQL, socket or pub/sub
    /// producer written under a mock or test-support tree of its service is
    /// tagged `mock`, read from the row's file relative to the service's own
    /// directory, as an HTTP route is. Both path shapes the two scan paths
    /// hand over are read: as scanned (prefixed by the repo path) and
    /// repo-relative (the incremental path's pub/sub keys). A file outside the
    /// service directory stays `route`, and a call keeps the default, as an
    /// HTTP call does.
    #[test]
    fn non_http_producers_under_a_mock_tree_are_tagged_mock() {
        use crate::operation::{EndpointProvenance, PubsubRole, SocketDirection};

        let service = Config {
            directory: Some("apps/api".to_string()),
            ..Config::default()
        };
        let mut graphql_producer = graphql_op(
            crate::operation::GraphqlOperationKind::Query,
            "partnerQuote",
            None,
        );
        graphql_producer.file_path = PathBuf::from("/repo/apps/api/src/mocks/partnerMock.ts");
        let mut socket_listener =
            socket_op("chat:send", SocketDirection::ClientToServer, None, None);
        socket_listener.file_path = PathBuf::from("/repo/apps/api/cypress/support/chat.ts");
        let bus = |event: &str, file: &str| crate::event_emitter::BusOp {
            key: OperationKey::pubsub(event),
            event: event.to_string(),
            file_path: PathBuf::from(file),
            line: 4,
        };
        let extractions = ProtocolExtractions {
            graphql: crate::graphql::GraphqlExtraction {
                producers: vec![graphql_producer],
                ..Default::default()
            },
            sockets: crate::socket_io::SocketExtraction {
                listeners: vec![socket_listener],
                emitters: vec![],
            },
            event_bus: crate::event_emitter::BusExtraction {
                subscribers: vec![
                    bus("job.done", "/repo/apps/api/src/jobs.ts"),
                    bus("job.stalled", "/repo/shared/mocks/jobs.ts"),
                ],
                publishers: vec![bus("job.retry", "/repo/apps/api/src/mocks/jobs.ts")],
            },
            library: Default::default(),
        };
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "apps/api/src/mocks/broker.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "payments.settled",
                    PubsubRole::Subscriber,
                    None,
                    None,
                )],
                ..Default::default()
            },
        );
        let mut cloud_data = repo_with_bundle("svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &file_results,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            "/repo",
            &service,
        );

        let provenance_of = |rows: &[ApiEndpointDetails], canonical: &str| {
            rows.iter()
                .find(|row| row.key.canonical() == canonical)
                .unwrap_or_else(|| panic!("no row for {canonical}"))
                .provenance
        };
        let endpoints = &cloud_data.endpoints;
        assert_eq!(
            provenance_of(endpoints, "graphql|query|partnerQuote"),
            EndpointProvenance::Mock
        );
        assert_eq!(
            provenance_of(endpoints, "socket|CLIENT->SERVER|chat:send"),
            EndpointProvenance::Mock,
            "a test-runner support tree is not product source"
        );
        assert_eq!(
            provenance_of(endpoints, "pubsub|payments.settled"),
            EndpointProvenance::Mock,
            "a repo-relative key is read against the service directory too"
        );
        assert_eq!(
            provenance_of(endpoints, "pubsub|job.done"),
            EndpointProvenance::Route
        );
        assert_eq!(
            provenance_of(endpoints, "pubsub|job.stalled"),
            EndpointProvenance::Route,
            "outside the service directory nothing is classified"
        );
        assert_eq!(
            provenance_of(&cloud_data.calls, "pubsub|job.retry"),
            EndpointProvenance::Route,
            "provenance is producer-side; a call keeps the default"
        );
    }

    /// Stage B1 merge: the file-analyzer's `graphql_operations` join their
    /// resolver location onto the SDL producer sharing the same canonical key.
    /// The SDL producer alone has no resolver location; after the merge it points
    /// at the resolver file/line so the producer can take the `FunctionReturn`
    /// infer path (its real response contract is the resolver's expanded return).
    #[test]
    fn merge_graphql_resolver_locations_joins_llm_op_onto_sdl_producer() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        // SDL producer `graphql|query|order` with no resolver location yet.
        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![
                graphql_op(GraphqlOperationKind::Query, "order", Some("Order")),
                // A second producer the LLM never reports a resolver for: it must
                // stay `None` (no spurious join).
                graphql_op(GraphqlOperationKind::Query, "orders", Some("[Order!]!")),
            ],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        // file_results keyed by path, carrying the matching LLM graphql_operation
        // plus one op (`createOrder`) with NO SDL producer (must be ignored).
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "packages/gateway/src/orders.resolver.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![
                    GraphqlOperation {
                        kind: GraphqlOperationKind::Query,
                        field: "order".to_string(),
                        resolver_function: Some("resolveOrder".to_string()),
                        resolver_line: Some(38),
                        primary_type_symbol: Some("ApiResponse".to_string()),
                        type_import_source: None,
                        // Even if the model ALSO emits a backing type here, the
                        // resolver path must win (regression guard: bundling the
                        // bare `Order` would drop the ApiResponse envelope).
                        backing_type_symbol: Some("Order".to_string()),
                        backing_type_source: None,
                    },
                    GraphqlOperation {
                        kind: GraphqlOperationKind::Mutation,
                        field: "createOrder".to_string(),
                        resolver_function: Some("createOrder".to_string()),
                        resolver_line: Some(7),
                        primary_type_symbol: None,
                        type_import_source: None,
                        backing_type_symbol: None,
                        backing_type_source: None,
                    },
                ],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        let order = graphql
            .producers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|order")
            .expect("order producer");
        assert_eq!(
            order.resolver_file,
            Some(PathBuf::from("packages/gateway/src/orders.resolver.ts")),
            "the resolver file must come from the file_results key"
        );
        assert_eq!(order.resolver_line, Some(38));
        // The SDL anchor is untouched by the merge.
        assert_eq!(order.primary_type_symbol.as_deref(), Some("Order"));
        // A resolver was matched, so the FunctionReturn path wins even though the
        // op also carries a `backing_type_symbol`: the type-locate fallback must
        // NOT fire (bundling the bare `Order` would drop the ApiResponse
        // envelope — the exact live-eval regression this guards against).
        assert_eq!(order.response_type_symbol, None);

        // The producer with no matching LLM op keeps both resolver fields None
        // and gains no type-locate fallback.
        let orders = graphql
            .producers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|orders")
            .expect("orders producer");
        assert_eq!(orders.resolver_file, None);
        assert_eq!(orders.resolver_line, None);
        assert_eq!(orders.response_type_symbol, None);

        // The LLM op with no SDL producer (`mutation createOrder`) created no
        // new producer — it was ignored.
        assert!(
            !graphql
                .producers
                .iter()
                .any(|op| op.key.canonical() == "graphql|mutation|createOrder"),
            "an LLM op with no matching SDL producer must not create a producer"
        );
    }

    /// #248: an SDL producer field with NO resolver function but a co-located
    /// backing type (the LLM emits `primary_type_symbol` with a null
    /// `resolver_function`) picks up the type-locate fallback — the scanner
    /// records the backing type so the sidecar can bundle + list-wrap it. The
    /// resolver locators stay `None` (no FunctionReturn), and `resolver_file` is
    /// still stamped with the file the entry came from.
    #[test]
    fn merge_graphql_type_locate_for_resolverless_field() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                GraphqlOperationKind::Query,
                "orders",
                Some("[Order!]!"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "packages/gateway/src/orders.resolver.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![GraphqlOperation {
                    kind: GraphqlOperationKind::Query,
                    field: "orders".to_string(),
                    // No resolver — the field is backed only by a co-located type,
                    // carried on the dedicated backing_type_symbol.
                    resolver_function: None,
                    resolver_line: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                    backing_type_symbol: Some("Order".to_string()),
                    backing_type_source: None,
                }],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        let orders = graphql
            .producers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|orders")
            .expect("orders producer");
        assert_eq!(
            orders.resolver_file,
            Some(PathBuf::from("packages/gateway/src/orders.resolver.ts")),
            "the file the entry came from is still recorded"
        );
        // No FunctionReturn: the resolver locators stay unset.
        assert_eq!(orders.resolver_line, None);
        // The type-locate fallback carries the backing type for the sidecar.
        assert_eq!(orders.response_type_symbol.as_deref(), Some("Order"));
        assert_eq!(orders.response_type_source, None);
        // The SDL anchor is untouched.
        assert_eq!(orders.primary_type_symbol.as_deref(), Some("[Order!]!"));
    }

    /// A whitespace-only `resolver_function` (e.g. `" "`) must be treated the
    /// same as `None`/empty: it is not a real function name, so it must not
    /// take the FunctionReturn path, and the type-locate fallback (backing
    /// type) must still fire.
    #[test]
    fn merge_graphql_whitespace_only_resolver_function_is_treated_as_absent() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                GraphqlOperationKind::Query,
                "orders",
                Some("[Order!]!"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "packages/gateway/src/orders.resolver.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![GraphqlOperation {
                    kind: GraphqlOperationKind::Query,
                    field: "orders".to_string(),
                    // Whitespace only, not a real resolver name.
                    resolver_function: Some("  ".to_string()),
                    resolver_line: Some(38),
                    primary_type_symbol: None,
                    type_import_source: None,
                    backing_type_symbol: Some("Order".to_string()),
                    backing_type_source: None,
                }],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        let orders = graphql
            .producers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|orders")
            .expect("orders producer");
        // No FunctionReturn: a whitespace-only name must not anchor a resolver.
        assert_eq!(orders.resolver_line, None);
        // The type-locate fallback still fires (the producer stays anchored).
        assert_eq!(orders.response_type_symbol.as_deref(), Some("Order"));
    }

    /// A named `resolver_function` whose `resolver_line` is unusable (absent,
    /// or non-positive so it clamps to `None`) must NOT take the
    /// FunctionReturn path with a dead locator:
    /// `collect_graphql_producer_infer_requests` requires BOTH `resolver_file`
    /// and `resolver_line`, so the op would emit no infer request, and the
    /// skipped backing-type fallback would emit no `SymbolRequest` either —
    /// the producer ends up with no type request of any kind. When the line
    /// is unusable, the backing-type fallback must fire instead.
    #[test]
    fn merge_graphql_unusable_resolver_line_falls_back_to_backing_type() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![
                graphql_op(GraphqlOperationKind::Query, "order", Some("Order")),
                graphql_op(GraphqlOperationKind::Query, "orders", Some("[Order!]!")),
            ],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "packages/gateway/src/orders.resolver.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![
                    // Named resolver, but the model omitted the line.
                    GraphqlOperation {
                        kind: GraphqlOperationKind::Query,
                        field: "order".to_string(),
                        resolver_function: Some("resolveOrder".to_string()),
                        resolver_line: None,
                        primary_type_symbol: None,
                        type_import_source: None,
                        backing_type_symbol: Some("Order".to_string()),
                        backing_type_source: None,
                    },
                    // Named resolver with a non-positive line (clamps to None).
                    GraphqlOperation {
                        kind: GraphqlOperationKind::Query,
                        field: "orders".to_string(),
                        resolver_function: Some("resolveOrders".to_string()),
                        resolver_line: Some(0),
                        primary_type_symbol: None,
                        type_import_source: None,
                        backing_type_symbol: Some("Order".to_string()),
                        backing_type_source: None,
                    },
                ],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        for field in ["order", "orders"] {
            let producer = graphql
                .producers
                .iter()
                .find(|op| op.key.canonical() == format!("graphql|query|{field}"))
                .expect("producer");
            // No FunctionReturn: an unusable line cannot anchor a resolver.
            assert_eq!(
                producer.resolver_line, None,
                "{field}: an unusable resolver_line must stay None"
            );
            // The backing-type fallback must fire so the producer stays anchored.
            assert_eq!(
                producer.response_type_symbol.as_deref(),
                Some("Order"),
                "{field}: the backing-type fallback must fire when the \
                 resolver line is unusable"
            );
        }

        // End to end: no dead FunctionReturn infer requests, and each producer
        // gets a backing-type SymbolRequest — a type request of SOME kind.
        let orchestrator = FileOrchestrator::new(AgentService::new());
        assert!(
            orchestrator
                .collect_graphql_producer_infer_requests(&graphql, ".", &modules_without_config())
                .is_empty(),
            "no resolver line means no FunctionReturn infer request"
        );
        let requests =
            orchestrator.collect_graphql_type_requests(&graphql, ".", &modules_without_config());
        assert_eq!(
            requests.len(),
            2,
            "each producer must emit a backing-type SymbolRequest, got: {:?}",
            requests
        );
    }

    /// carrick#1761: a consumer alias is keyed by its operation alone (#291),
    /// so one anchor answers for every row of a field. A row whose executed
    /// document declares the field's type answers it ahead of a locate on
    /// another row of the same field, through ONE infer request at the
    /// declared property's type, in the sidecar's UTF-16 numbering. An
    /// explicit call-site generic on any row keeps its alias on the symbol
    /// path.
    #[test]
    fn a_declared_result_type_answers_its_alias_ahead_of_a_locate() {
        use crate::graphql_document_sites::DeclaredFieldType;
        use crate::operation::GraphqlOperationKind::Query;

        let tmp = tempfile::tempdir().unwrap();
        let module = tmp.path().join("graphql.ts");
        // Multi-byte prose above the span, so a byte offset sent unconverted
        // lands past the type.
        let source = "// Généré — ne pas modifier\nexport type InvoiceQuery = { invoice: { id: string } | null };\n";
        std::fs::write(&module, source).unwrap();
        let text = "{ id: string } | null";
        let byte_start = source.find(text).unwrap() as u32;
        let declared = DeclaredFieldType {
            file: module.clone(),
            line: 2,
            lo: byte_start + crate::swc_scanner::SWC_SPAN_BASE,
            hi: byte_start + text.len() as u32 + crate::swc_scanner::SWC_SPAN_BASE,
        };
        let consumer = |field: &str, file: &str, line: u32| crate::graphql::GraphqlOp {
            file_path: PathBuf::from(file),
            line,
            document_line: line,
            primary_type_symbol: None,
            ..graphql_op(Query, field, None)
        };
        let mut located = consumer("invoice", "src/b.tsx", 9);
        located.consumer_located_type_symbol = Some("InvoiceQuery".to_string());
        let mut declared_row = consumer("invoice", "src/a.tsx", 4);
        declared_row.declared_result_type = Some(declared.clone());
        let mut generic = consumer("order", "src/c.tsx", 3);
        generic.payload_type_symbol = Some("OrderView".to_string());
        let mut declared_order = consumer("order", "src/d.tsx", 5);
        declared_order.declared_result_type = Some(declared.clone());
        let graphql = crate::graphql::GraphqlExtraction {
            consumers: vec![located, declared_row, generic, declared_order],
            ..Default::default()
        };

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let alias = |field: &str| {
            crate::type_manifest::build_manifest_type_alias(
                &OperationKey::graphql(Query, field),
                crate::cloud_storage::ManifestRole::Consumer,
                crate::cloud_storage::ManifestTypeKind::Response,
            )
        };
        let symbols: Vec<(String, Option<String>)> = orchestrator
            .collect_graphql_type_requests(&graphql, ".", &modules_without_config())
            .into_iter()
            .map(|request| (request.symbol_name, request.alias))
            .collect();
        assert_eq!(
            symbols,
            vec![("OrderView".to_string(), Some(alias("order")))],
            "the declared alias sends no located symbol; the generic keeps its own"
        );

        let infer = orchestrator.collect_graphql_consumer_infer_requests(&graphql);
        assert_eq!(
            infer.len(),
            1,
            "one request, for the declared alias: {infer:?}"
        );
        let request = &infer[0];
        assert_eq!(request.alias, Some(alias("invoice")));
        assert_eq!(request.infer_kind, InferKind::Expression);
        assert_eq!(request.file_path, module.to_string_lossy());
        let utf16_start = source[..byte_start as usize].encode_utf16().count() as u32;
        assert_eq!(
            (request.span_start, request.span_end),
            (
                Some(utf16_start),
                Some(utf16_start + text.encode_utf16().count() as u32)
            ),
            "the span goes out in the sidecar's numbering"
        );
    }

    /// carrick#1760: a located type that is the result of the row's whole
    /// operation was read at the row's field. The row never bundles that
    /// symbol and its manifest entry names none: it reads the field's
    /// property through an infer request, as a declared field type does. A
    /// document's declaration still answers its alias first, and a located
    /// payload type keeps the symbol path.
    #[test]
    fn a_located_operation_result_is_read_at_its_field_not_bundled() {
        use crate::graphql_document_sites::DeclaredFieldType;
        use crate::operation::GraphqlOperationKind::Query;
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().unwrap();
        let module = tmp.path().join("types.ts");
        let source = "export type PageQuery = { invoice: { id: string } | null, settings: { prefix: string } };\nexport type SettingsQuery = { settings: { prefix: string; code: number } };\n";
        std::fs::write(&module, source).unwrap();
        let span = |text: &str| {
            let start = source.find(text).unwrap() as u32;
            DeclaredFieldType {
                file: module.clone(),
                line: 1,
                lo: start + crate::swc_scanner::SWC_SPAN_BASE,
                hi: start + text.len() as u32 + crate::swc_scanner::SWC_SPAN_BASE,
            }
        };
        let consumer = |field: &str, file: &str, line: u32| crate::graphql::GraphqlOp {
            file_path: PathBuf::from(file),
            line,
            document_line: line,
            primary_type_symbol: None,
            ..graphql_op(Query, field, None)
        };
        let mut invoice = consumer("invoice", "src/a.tsx", 4);
        invoice.consumer_located_type_symbol = Some("PageQuery".to_string());
        invoice.located_field_type = Some(span("{ id: string } | null"));
        // First by file and line, but a located type ranks after a declared one.
        let mut settings_located = consumer("settings", "src/a.tsx", 4);
        settings_located.consumer_located_type_symbol = Some("PageQuery".to_string());
        settings_located.located_field_type = Some(span("{ prefix: string }"));
        let mut settings_declared = consumer("settings", "src/b.tsx", 7);
        settings_declared.declared_result_type = Some(span("{ prefix: string; code: number }"));
        let mut note = consumer("note", "src/c.tsx", 2);
        note.consumer_located_type_symbol = Some("NoteView".to_string());
        note.consumer_located_type_source = Some("./types".to_string());
        let graphql = crate::graphql::GraphqlExtraction {
            consumers: vec![invoice, settings_located, settings_declared, note],
            ..Default::default()
        };

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let alias = |field: &str| {
            crate::type_manifest::build_manifest_type_alias(
                &OperationKey::graphql(Query, field),
                crate::cloud_storage::ManifestRole::Consumer,
                crate::cloud_storage::ManifestTypeKind::Response,
            )
        };
        let symbols: Vec<(String, Option<String>)> = orchestrator
            .collect_graphql_type_requests(&graphql, ".", &modules_without_config())
            .into_iter()
            .map(|request| (request.symbol_name, request.alias))
            .collect();
        assert_eq!(
            symbols,
            vec![("NoteView".to_string(), Some(alias("note")))],
            "the operation's result type is never bundled; the payload type is"
        );

        let infer: BTreeMap<Option<String>, (Option<u32>, Option<u32>)> = orchestrator
            .collect_graphql_consumer_infer_requests(&graphql)
            .into_iter()
            .map(|request| {
                assert_eq!(request.infer_kind, InferKind::Expression);
                assert_eq!(request.file_path, module.to_string_lossy());
                (request.alias, (request.span_start, request.span_end))
            })
            .collect();
        let sidecar_span = |text: &str| {
            let start = source.find(text).unwrap() as u32;
            (Some(start), Some(start + text.len() as u32))
        };
        assert_eq!(
            infer,
            BTreeMap::from([
                (
                    Some(alias("invoice")),
                    sidecar_span("{ id: string } | null")
                ),
                (
                    Some(alias("settings")),
                    sidecar_span("{ prefix: string; code: number }")
                ),
            ]),
            "the located row answers its alias; the declaration answers ahead of it"
        );

        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql,
            sockets: Default::default(),
            library: Default::default(),
        };
        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());
        let symbols: Vec<(String, Option<String>)> = entries
            .iter()
            .map(|entry| (entry.file_path.clone(), entry.primary_type_symbol.clone()))
            .collect();
        assert_eq!(
            symbols,
            vec![
                ("src/a.tsx".to_string(), None),
                ("src/a.tsx".to_string(), None),
                ("src/b.tsx".to_string(), None),
                ("src/c.tsx".to_string(), Some("NoteView".to_string())),
            ],
            "a row read at its field names no symbol for the operation's result"
        );
    }

    /// A minimal endpoint the file-analyzer would emit alongside a
    /// misattributed resolver claim: only `handler_name` matters to the
    /// borrow witness.
    fn endpoint_with_handler(handler_name: &str) -> EndpointResult {
        EndpointResult {
            handler_declaration_line: None,
            registration_literal: None,
            view_module: false,
            candidate_id: "span:1-2".to_string(),
            line_number: 7,
            owner_node: "TicketsController".to_string(),
            method: "GET".to_string(),
            path: "/tickets/:id".to_string(),
            handler_name: handler_name.to_string(),
            pattern_matched: "@Get(\":id\")".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: None,
            response_expression_line: None,
            emission_style: None,
            primary_type_symbol: None,
            type_import_source: None,
            resolution_source: None,
            dispatch: None,
        }
    }

    /// The corpus-3 `query ticket` live-eval false-incompatible, reduced: the
    /// file-analyzer links an HTTP endpoint HANDLER (`findOne`, which returns
    /// a ticket-shaped object) as the field's resolver from the controller
    /// file, while the real resolver file claims `resolveTicket`. The borrow
    /// witness — the claimed function is an `endpoints[].handler_name` in the
    /// SAME file's analysis — must drop the controller claim deterministically,
    /// so the resolver file's claim wins in every run (never a `HashMap`-order
    /// coin flip whose losing side rides the wrong inferred return into a
    /// confidently wrong verdict).
    #[test]
    fn merge_graphql_resolver_locations_drops_http_handler_claims() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                GraphqlOperationKind::Query,
                "ticket",
                Some("Ticket"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        let claim = |function: &str, line: i32| GraphqlOperation {
            kind: GraphqlOperationKind::Query,
            field: "ticket".to_string(),
            resolver_function: Some(function.to_string()),
            resolver_line: Some(line),
            primary_type_symbol: None,
            type_import_source: None,
            backing_type_symbol: None,
            backing_type_source: None,
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        // Sorted-order note: "src/tickets.controller.ts" sorts BEFORE
        // "src/tickets.resolver.ts", so without the witness the controller
        // claim would be the accepted first claim and the resolver claim would
        // read as a conflict — the assertion below fails either way unless the
        // witness drops the controller claim outright.
        file_results.insert(
            "src/tickets.controller.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![endpoint_with_handler("findOne")],
                data_calls: vec![],
                graphql_operations: vec![claim("findOne", 8)],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );
        file_results.insert(
            "src/tickets.resolver.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![claim("resolveTicket", 4)],
                pubsub_operations: vec![],
                dispatch_tables: Vec::new(),
            },
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        let ticket = &graphql.producers[0];
        assert_eq!(
            ticket.resolver_file,
            Some(PathBuf::from("src/tickets.resolver.ts")),
            "the HTTP-handler claim must be dropped; the resolver file's claim wins"
        );
        assert_eq!(ticket.resolver_line, Some(4));
        // The dropped claim must not smuggle in a backing type either.
        assert_eq!(ticket.response_type_symbol, None);
    }

    /// Two files claim DIFFERENT resolver locations for the same producer key
    /// and neither claim is witnessed as an HTTP handler: the location is
    /// ambiguous, and ambiguity fails closed — the producer stays unlocated
    /// (no FunctionReturn infer request, type_state stays `Unknown`, its pairs
    /// verdict unverifiable) instead of the previous last-write-wins over
    /// nondeterministic `HashMap` iteration order, where the losing run
    /// shipped the WRONG file's inferred return as the producer's contract.
    #[test]
    fn merge_graphql_resolver_locations_conflicting_claims_fail_closed() {
        use crate::agents::file_analyzer_agent::GraphqlOperation;
        use crate::operation::GraphqlOperationKind;

        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                GraphqlOperationKind::Query,
                "ticket",
                Some("Ticket"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };

        let claim = |function: &str, line: i32| GraphqlOperation {
            kind: GraphqlOperationKind::Query,
            field: "ticket".to_string(),
            resolver_function: Some(function.to_string()),
            resolver_line: Some(line),
            primary_type_symbol: None,
            type_import_source: None,
            backing_type_symbol: None,
            backing_type_source: None,
        };
        let result_with = |ops: Vec<GraphqlOperation>| FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls: vec![],
            graphql_operations: ops,
            pubsub_operations: vec![],
            dispatch_tables: Vec::new(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "src/a.resolver.ts".to_string(),
            result_with(vec![claim("resolveTicketA", 4)]),
        );
        file_results.insert(
            "src/b.resolver.ts".to_string(),
            result_with(vec![claim("resolveTicketB", 9)]),
        );

        merge_graphql_resolver_locations(&mut graphql, &file_results);

        let ticket = &graphql.producers[0];
        assert_eq!(
            ticket.resolver_file, None,
            "conflicting claims must clear the location, not race for it"
        );
        assert_eq!(ticket.resolver_line, None);
        assert_eq!(ticket.response_type_symbol, None);
        // The SDL anchor is untouched by the conflict handling.
        assert_eq!(ticket.primary_type_symbol.as_deref(), Some("Ticket"));

        // Duplicate claims that AGREE are not a conflict: the shared claim is
        // applied.
        let mut agreeing = crate::graphql::GraphqlExtraction {
            producers: vec![graphql_op(
                GraphqlOperationKind::Query,
                "ticket",
                Some("Ticket"),
            )],
            consumers: vec![],
            input_declarations: Default::default(),
        };
        let mut agreeing_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        agreeing_results.insert(
            "src/a.resolver.ts".to_string(),
            result_with(vec![claim("resolveTicket", 4), claim("resolveTicket", 4)]),
        );
        merge_graphql_resolver_locations(&mut agreeing, &agreeing_results);
        assert_eq!(
            agreeing.producers[0].resolver_file,
            Some(PathBuf::from("src/a.resolver.ts"))
        );
        assert_eq!(agreeing.producers[0].resolver_line, Some(4));
    }

    /// #268 isolation guard: a consumer op with an explicit call-site generic
    /// (`payload_type_symbol` already set by the deterministic
    /// `TaggedTplVisitor::capture_request_call` pass) must keep that anchor
    /// even when a stray `graphql_consumer_locates` entry also matches its
    /// `(file_path, kind, field)` — mirrors 186cb27's resolver-first
    /// regression guard on the producer side.
    #[test]
    fn merge_graphql_consumer_locations_never_overrides_explicit_generic() {
        use crate::agents::file_analyzer_agent::GraphqlConsumerLocate;
        use crate::operation::GraphqlOperationKind;

        let anchored = graphql_consumer_op_at(
            GraphqlOperationKind::Query,
            "order",
            "web-frontend/lib/graphql.ts",
            Some("OrderView"),
        );
        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![anchored],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "web-frontend/lib/graphql.ts".to_string(),
            FileAnalysisResult {
                // A stray/hallucinated locate entry for an op that is already
                // anchored — must be ignored.
                graphql_consumer_locates: vec![GraphqlConsumerLocate {
                    kind: GraphqlOperationKind::Query,
                    field: "order".to_string(),
                    result_type_symbol: "StrayType".to_string(),
                    result_type_source: None,
                }],
                ..Default::default()
            },
        );

        merge_graphql_consumer_locations(&mut graphql, &file_results, "");

        let order = &graphql.consumers[0];
        assert_eq!(
            order.payload_type_symbol.as_deref(),
            Some("OrderView"),
            "the explicit call-site anchor must be untouched"
        );
        assert_eq!(
            order.consumer_located_type_symbol, None,
            "a stray locate entry must NEVER populate the fallback when the op \
             is already anchored"
        );
    }

    /// #268 per-file join proof: the SAME `(kind, field)` consumed from TWO
    /// different files (fan-in) must each get their OWN located type — the
    /// join is keyed on `(file_path, kind, field)`, not the canonical key
    /// alone. A canonical-key-only map (the producer merge's approach) would
    /// collide every file's locate entry onto whichever consumer op happened
    /// to occupy that key first; this is the load-bearing difference from
    /// `merge_graphql_resolver_locations`.
    #[test]
    fn merge_graphql_consumer_locations_scopes_fan_in_per_file() {
        use crate::agents::file_analyzer_agent::GraphqlConsumerLocate;
        use crate::operation::GraphqlOperationKind;

        let consumer_a = graphql_consumer_op_at(
            GraphqlOperationKind::Subscription,
            "orderUpdated",
            "web-frontend/lib/graphql.ts",
            None,
        );
        let consumer_b = graphql_consumer_op_at(
            GraphqlOperationKind::Subscription,
            "orderUpdated",
            "admin-dashboard/lib/graphql.ts",
            None,
        );
        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![consumer_a, consumer_b],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "web-frontend/lib/graphql.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![GraphqlConsumerLocate {
                    kind: GraphqlOperationKind::Subscription,
                    field: "orderUpdated".to_string(),
                    result_type_symbol: "OrderUpdate".to_string(),
                    result_type_source: None,
                }],
                ..Default::default()
            },
        );
        file_results.insert(
            "admin-dashboard/lib/graphql.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![GraphqlConsumerLocate {
                    kind: GraphqlOperationKind::Subscription,
                    field: "orderUpdated".to_string(),
                    result_type_symbol: "AdminOrderUpdate".to_string(),
                    result_type_source: None,
                }],
                ..Default::default()
            },
        );

        merge_graphql_consumer_locations(&mut graphql, &file_results, "");

        let web = graphql
            .consumers
            .iter()
            .find(|op| op.file_path == Path::new("web-frontend/lib/graphql.ts"))
            .expect("web-frontend consumer");
        assert_eq!(
            web.consumer_located_type_symbol.as_deref(),
            Some("OrderUpdate"),
            "web-frontend must get its OWN located type"
        );
        let admin = graphql
            .consumers
            .iter()
            .find(|op| op.file_path == Path::new("admin-dashboard/lib/graphql.ts"))
            .expect("admin-dashboard consumer");
        assert_eq!(
            admin.consumer_located_type_symbol.as_deref(),
            Some("AdminOrderUpdate"),
            "admin-dashboard must get its OWN located type, not web-frontend's"
        );
    }

    /// carrick#1725: the op carries the path the scan discovered, absolute
    /// under an absolute repo path, while the incremental path keys the
    /// replayed answers repo-relative. Both forms of the answer's key must
    /// meet the op, and a file outside the repo must not.
    #[test]
    fn merge_graphql_consumer_locations_joins_absolute_ops_to_either_key_form() {
        use crate::agents::file_analyzer_agent::GraphqlConsumerLocate;
        use crate::operation::GraphqlOperationKind;

        let locate = |symbol: &str| FileAnalysisResult {
            graphql_consumer_locates: vec![GraphqlConsumerLocate {
                kind: GraphqlOperationKind::Subscription,
                field: "orderUpdated".to_string(),
                result_type_symbol: symbol.to_string(),
                result_type_source: None,
            }],
            ..Default::default()
        };
        let located = |key: &str, repo: &str| {
            let mut graphql = crate::graphql::GraphqlExtraction {
                producers: vec![],
                consumers: vec![graphql_consumer_op_at(
                    GraphqlOperationKind::Subscription,
                    "orderUpdated",
                    "/work/web/lib/graphql.ts",
                    None,
                )],
                input_declarations: Default::default(),
            };
            let file_results = HashMap::from([(key.to_string(), locate("OrderUpdate"))]);
            merge_graphql_consumer_locations(&mut graphql, &file_results, repo);
            graphql.consumers[0].consumer_located_type_symbol.clone()
        };

        assert_eq!(
            located("lib/graphql.ts", "/work/web").as_deref(),
            Some("OrderUpdate"),
            "the incremental path's repo-relative key"
        );
        assert_eq!(
            located("/work/web/lib/graphql.ts", "/work/web/").as_deref(),
            Some("OrderUpdate"),
            "the full path's discovered key, with a trailing slash on the repo"
        );
        assert_eq!(
            located("lib/graphql.ts", "/work/other"),
            None,
            "an op outside the repo stays absolute and meets no relative key"
        );
    }

    /// #268: a `graphql_consumer_locates` entry with no matching consumer op
    /// (wrong file, or wrong kind/field) is ignored — it must not create a new
    /// consumer op, touch an unrelated one, or panic.
    #[test]
    fn merge_graphql_consumer_locations_ignores_unmatched_locate_entry() {
        use crate::agents::file_analyzer_agent::GraphqlConsumerLocate;
        use crate::operation::GraphqlOperationKind;

        let consumer = graphql_consumer_op_at(
            GraphqlOperationKind::Query,
            "order",
            "web-frontend/lib/graphql.ts",
            None,
        );
        let mut graphql = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![consumer],
            input_declarations: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "web-frontend/lib/graphql.ts".to_string(),
            FileAnalysisResult {
                // A locate entry for a field with no matching consumer op in
                // this file.
                graphql_consumer_locates: vec![GraphqlConsumerLocate {
                    kind: GraphqlOperationKind::Mutation,
                    field: "refundOrder".to_string(),
                    result_type_symbol: "RefundReceipt".to_string(),
                    result_type_source: None,
                }],
                ..Default::default()
            },
        );

        merge_graphql_consumer_locations(&mut graphql, &file_results, "");

        assert_eq!(graphql.consumers.len(), 1, "no new consumer op was created");
        assert_eq!(
            graphql.consumers[0].consumer_located_type_symbol, None,
            "the unrelated `order` consumer must not pick up the unmatched entry"
        );
    }

    /// A typed socket emitter produces a Response-kind manifest entry keyed by
    /// the socket OperationKey, carrying the captured payload symbol as the
    /// anchor. A GraphQL SDL producer carries its deterministic SDL type
    /// expression as the anchor (#248); a document consumer has no SDL type so
    /// its anchor stays `None`.
    #[test]
    fn protocol_manifest_entries_anchor_sockets_and_graphql_producers() {
        use crate::operation::{GraphqlOperationKind, SocketDirection};

        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction {
                producers: vec![graphql_op(
                    GraphqlOperationKind::Query,
                    "order",
                    Some("Order"),
                )],
                consumers: vec![graphql_op(GraphqlOperationKind::Query, "order", None)],
                input_declarations: Default::default(),
            },
            sockets: crate::socket_io::SocketExtraction {
                listeners: vec![],
                emitters: vec![socket_op(
                    "payment:settled",
                    SocketDirection::ServerToClient,
                    Some("Payment"),
                    Some("./types/payment"),
                )],
            },
            library: Default::default(),
        };

        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());

        let socket_entry = entries
            .iter()
            .find(|e| e.key.canonical() == "socket|SERVER->CLIENT|payment:settled")
            .expect("socket manifest entry");
        assert_eq!(socket_entry.role, ManifestRole::Consumer);
        assert_eq!(socket_entry.type_kind, ManifestTypeKind::Response);
        assert_eq!(socket_entry.primary_type_symbol.as_deref(), Some("Payment"));
        // One entry per op — no phantom Request alias.
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.key.canonical() == "socket|SERVER->CLIENT|payment:settled")
                .count(),
            1
        );

        // The SDL producer carries its deterministic SDL-type anchor (#248).
        let graphql_producer = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "graphql|query|order" && e.role == ManifestRole::Producer
            })
            .expect("graphql producer manifest entry");
        assert_eq!(
            graphql_producer.primary_type_symbol.as_deref(),
            Some("Order"),
            "the SDL producer anchor must be the field's SDL type expression"
        );
        assert!(
            !graphql_producer.type_alias.is_empty(),
            "graphql op must get a stable type_alias"
        );
        assert_eq!(graphql_producer.type_state, ManifestTypeState::Unknown);

        // The document consumer has no SDL type, so its anchor stays unset.
        let graphql_consumer = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "graphql|query|order" && e.role == ManifestRole::Consumer
            })
            .expect("graphql consumer manifest entry");
        assert_eq!(graphql_consumer.primary_type_symbol, None);
    }

    /// An SDL root field that declares arguments gets a Request-kind producer
    /// entry carrying the schema's own statement of the request (carrick#1158):
    /// the definition `get_type_definition` serves by alias, and by the input
    /// type's name through `primary_type_symbol`. A field without arguments and
    /// a document consumer still get no Request entry.
    #[test]
    fn sdl_arguments_become_a_request_manifest_entry_on_the_wire() {
        let sdl = r#"
            type Mutation { createInvoice(input: CreateInvoiceInput!): Invoice! }
            type Query { health: String! }
            input CreateInvoiceInput { customerId: ID!, total: Int! }
        "#;
        let mut graphql =
            crate::graphql::extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        graphql.consumers = crate::graphql::extract_from_document_text(
            "mutation { createInvoice(input: $i) { id } }",
            Path::new("web/q.graphql"),
            1,
        )
        .consumers;
        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql,
            sockets: crate::socket_io::SocketExtraction::default(),
            library: Default::default(),
        };

        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());

        let requests: Vec<&TypeManifestEntry> = entries
            .iter()
            .filter(|e| e.type_kind == ManifestTypeKind::Request)
            .collect();
        assert_eq!(requests.len(), 1, "got: {entries:#?}");
        let request = requests[0];
        assert_eq!(request.role, ManifestRole::Producer);
        assert_eq!(request.type_state, ManifestTypeState::Explicit);
        assert_eq!(
            request.type_alias,
            crate::type_manifest::build_manifest_type_alias_with_site_id(
                &request.key,
                ManifestRole::Producer,
                ManifestTypeKind::Request,
                None,
            )
        );

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["protocol"], "graphql");
        assert_eq!(json["field"], "createInvoice");
        assert_eq!(json["type_kind"], "request");
        assert_eq!(json["role"], "producer");
        assert_eq!(json["primary_type_symbol"], "CreateInvoiceInput");
        let definition = json["resolved_definition"].as_str().unwrap().to_string();
        assert!(
            definition.starts_with(
                "createInvoice(input: CreateInvoiceInput!)\n\ninput CreateInvoiceInput {"
            ),
            "got: {definition}"
        );

        // A reader holding this row reads it back unchanged: no field is new,
        // only the row is.
        let back: TypeManifestEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back.resolved_definition, Some(definition));
    }

    /// The fragile contract the whole anchor join hinges on: the alias on the
    /// socket manifest entry MUST equal the alias on the SymbolRequest, both
    /// computed by `build_manifest_type_alias(key, role, Response)`. If they
    /// ever diverge the resolved `.d.ts` never joins back and the entry stays
    /// `Unknown` — silently. (Plan test #3.)
    #[test]
    fn socket_symbol_request_alias_matches_manifest_alias() {
        use crate::operation::SocketDirection;

        let emitter = socket_op(
            "payment:settled",
            SocketDirection::ServerToClient,
            Some("Payment"),
            Some("./types/payment"),
        );
        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction::default(),
            sockets: crate::socket_io::SocketExtraction {
                listeners: vec![],
                emitters: vec![emitter.clone()],
            },
            library: Default::default(),
        };

        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());
        let manifest_alias = entries
            .iter()
            .find(|e| e.key.canonical() == "socket|SERVER->CLIENT|payment:settled")
            .map(|e| e.type_alias.clone())
            .expect("socket manifest entry");

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_socket_type_requests(
            &extractions.sockets,
            ".",
            &modules_without_config(),
        );
        let request = requests
            .iter()
            .find(|r| r.symbol_name == "Payment")
            .expect("socket SymbolRequest");

        assert_eq!(
            request.alias.as_deref(),
            Some(manifest_alias.as_str()),
            "SymbolRequest.alias must byte-match the manifest entry's alias \
             (both build_manifest_type_alias(key, Consumer, Response)) or the \
             enrich-join silently breaks"
        );
        // Independently confirm both equal the canonical builder output.
        let expected = crate::type_manifest::build_manifest_type_alias(
            &emitter.key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
        );
        assert_eq!(manifest_alias, expected);
        assert_eq!(request.alias.as_deref(), Some(expected.as_str()));
    }

    /// The same fragile alias contract for pub/sub (PR-6, corpus-2 resolution dim):
    /// the SymbolRequest alias produced by `collect_pubsub_type_requests` MUST
    /// byte-match the manifest entry's alias from `append_pubsub_manifest_entries`,
    /// or the resolved payload `.d.ts` never joins back and the op stays
    /// `Unknown` — silently. A subscriber is the producer side.
    #[test]
    fn pubsub_symbol_request_alias_matches_manifest_alias() {
        use crate::operation::PubsubRole;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "web-dashboard/lib/realtime.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "metrics.page_view",
                    PubsubRole::Subscriber,
                    Some("PageView"),
                    Some("./types/metrics"),
                )],
                ..Default::default()
            },
        );

        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );
        let manifest_alias = entries
            .iter()
            .find(|e| e.key.canonical() == "pubsub|metrics.page_view")
            .map(|e| e.type_alias.clone())
            .expect("pubsub manifest entry");

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_pubsub_type_requests(
            &file_results,
            ".",
            &modules_without_config(),
        );
        let request = requests
            .iter()
            .find(|r| r.symbol_name == "PageView")
            .expect("pubsub SymbolRequest");

        assert_eq!(
            request.alias.as_deref(),
            Some(manifest_alias.as_str()),
            "pubsub SymbolRequest.alias must byte-match the manifest entry's alias \
             or the resolution enrich-join silently breaks"
        );
        // Independently confirm both equal the canonical builder output
        // (subscriber = producer side).
        let expected = crate::type_manifest::build_manifest_type_alias(
            &OperationKey::pubsub("metrics.page_view"),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
        );
        assert_eq!(manifest_alias, expected);
        assert_eq!(request.alias.as_deref(), Some(expected.as_str()));
    }

    /// Fan-in regression (the corpus-2 compat false-negative): two publishers on
    /// the SAME topic but in different repos/files must get DISTINCT consumer
    /// aliases. They previously hashed to one alias (`build_manifest_type_alias`
    /// keys only on `topic|consumer|Response`), so the merged consumer
    /// declarations defined that interface twice with different bodies
    /// — one publisher's payload type masked the other's and the masked edge
    /// reported a spurious compat mismatch. The publisher alias now disambiguates
    /// by call site, and each publisher's SymbolRequest alias still byte-matches
    /// its own manifest entry so the resolution join holds.
    #[test]
    fn pubsub_fan_in_publishers_get_distinct_consumer_aliases() {
        use crate::operation::PubsubRole;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        // Two repos publishing `order.placed` with a same-named `OrderPlaced`
        // symbol whose definition differs per repo — the exact corpus-2 shape.
        file_results.insert(
            "orders-engine/src/publish.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "order.placed",
                    PubsubRole::Publisher,
                    Some("OrderPlaced"),
                    Some("./types/order"),
                )],
                ..Default::default()
            },
        );
        file_results.insert(
            "billing-svc/src/emit.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "order.placed",
                    PubsubRole::Publisher,
                    Some("OrderPlaced"),
                    Some("./types/order"),
                )],
                ..Default::default()
            },
        );

        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );
        let manifest_aliases: HashSet<String> = entries
            .iter()
            .filter(|e| e.key.canonical() == "pubsub|order.placed")
            .map(|e| e.type_alias.clone())
            .collect();
        assert_eq!(
            manifest_aliases.len(),
            2,
            "two fan-in publishers must yield two DISTINCT consumer aliases, not \
             one collided alias (which masks one payload type at check time)"
        );

        // Each publisher's SymbolRequest alias must byte-match its manifest alias
        // (same call site → same call_id), so the resolution enrich-join holds.
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_pubsub_type_requests(
            &file_results,
            ".",
            &modules_without_config(),
        );
        let request_aliases: HashSet<String> =
            requests.iter().filter_map(|r| r.alias.clone()).collect();
        assert_eq!(
            request_aliases, manifest_aliases,
            "each publisher's SymbolRequest alias must byte-match its manifest \
             entry's alias across the fan-in set"
        );
    }

    /// The same fragile alias contract for the pub/sub INFER path (wrapper
    /// patterns whose payload type is generic-bound, never a named symbol):
    /// the `InferRequestItem` alias produced by `collect_pubsub_infer_requests`
    /// MUST byte-match the manifest entry's alias from
    /// `append_pubsub_manifest_entries` — same plain alias for subscribers
    /// (producers), same call-site-disambiguated alias for publishers
    /// (consumers) — or the resolved payload type never joins back and the op
    /// stays `Unknown`, silently. Also pins the role → InferKind routing, the
    /// two-anchor co-emission (#413: a named anchor no longer suppresses the
    /// infer request — the sidecar arbitrates), and the envelope-copy guard.
    #[test]
    fn pubsub_infer_request_alias_matches_manifest_alias() {
        use crate::operation::PubsubRole;
        use crate::services::type_sidecar::InferKind;

        let locator_op = |topic: &str, role: PubsubRole, line: i32, text: &str| {
            crate::agents::file_analyzer_agent::PubsubOperation {
                topic: topic.to_string(),
                role: Some(role),
                line_number: line,
                primary_type_symbol: None,
                type_import_source: None,
                broker: None,
                payload_expression_text: Some(text.to_string()),
                payload_expression_line: Some(line),
                backfilled: false,
            }
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "relay/src/relay.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![locator_op(
                    "itemArchived",
                    PubsubRole::Subscriber,
                    13,
                    "{ time, item }",
                )],
                ..Default::default()
            },
        );
        file_results.insert(
            "dispatch/src/dispatch.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![
                    locator_op("itemArchived", PubsubRole::Publisher, 9, "event"),
                    // Named anchor present WITH a usable locator → the op
                    // co-emits an infer request (#413) so the sidecar can
                    // arbitrate the two anchors; the explicit bundle still
                    // wins unless a borrow witness plus a root disagreement
                    // demotes it.
                    crate::agents::file_analyzer_agent::PubsubOperation {
                        topic: "orders.placed".to_string(),
                        role: Some(PubsubRole::Publisher),
                        line_number: 30,
                        primary_type_symbol: Some("OrderPlaced".to_string()),
                        type_import_source: None,
                        broker: None,
                        payload_expression_text: Some("order".to_string()),
                        payload_expression_line: Some(30),
                        backfilled: false,
                    },
                    // Envelope-copy guard: the locator text contains the op's
                    // own topic literal (the model copied the whole enqueue
                    // options object), so it must be dropped — an envelope's
                    // type on the manifest is a false compat verdict waiting
                    // to happen; Unknown is recoverable.
                    locator_op(
                        "records.reindex",
                        PubsubRole::Publisher,
                        41,
                        "{ id, job: \"records.reindex\", payload: { resourceId } }",
                    ),
                ],
                ..Default::default()
            },
        );

        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );
        let manifest_aliases: HashMap<ManifestRole, String> = entries
            .iter()
            .filter(|e| e.key.canonical() == "pubsub|itemArchived")
            .map(|e| (e.role, e.type_alias.clone()))
            .collect();

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_pubsub_infer_requests(&file_results, ".");

        // Co-emission + envelope guard: the two locator-anchored itemArchived
        // ops AND the named-anchor orders.placed op produce requests (#413);
        // only the topic-containing envelope copy is excluded.
        assert_eq!(requests.len(), 3, "requests: {requests:?}");
        assert!(
            requests
                .iter()
                .any(|r| r.expression_text.as_deref() == Some("order")),
            "an op with a primary_type_symbol and a usable locator must ALSO \
             emit an infer request so the sidecar can arbitrate the anchors"
        );
        assert!(
            !requests.iter().any(|r| r
                .expression_text
                .as_deref()
                .is_some_and(|t| t.contains("records.reindex"))),
            "a locator containing the op's topic literal is an envelope copy \
             and must be dropped, not resolved"
        );

        // Subscriber (producer side): FunctionParam, param_name = locator text,
        // plain alias byte-matching the manifest.
        let subscriber = requests
            .iter()
            .find(|r| r.infer_kind == InferKind::FunctionParam)
            .expect("subscriber infer request");
        assert_eq!(subscriber.param_name.as_deref(), Some("{ time, item }"));
        assert_eq!(
            subscriber.alias.as_deref(),
            manifest_aliases
                .get(&ManifestRole::Producer)
                .map(|s| s.as_str()),
            "subscriber infer alias must byte-match the Producer manifest alias"
        );

        // Publisher (consumer side): Expression, expression_text = locator
        // text, call-site alias byte-matching the manifest.
        let publisher = requests
            .iter()
            .find(|r| r.infer_kind == InferKind::Expression)
            .expect("publisher infer request");
        assert_eq!(publisher.expression_text.as_deref(), Some("event"));
        assert_eq!(
            publisher.alias.as_deref(),
            manifest_aliases
                .get(&ManifestRole::Consumer)
                .map(|s| s.as_str()),
            "publisher infer alias must byte-match the Consumer manifest alias \
             (same build_site_id over the same path/line/key)"
        );
    }

    /// A publisher locator whose `payload_expression_line` the model omitted
    /// must still anchor the sidecar's text search: the collector defaults
    /// `expression_line` to the operation's own line. An unanchored request
    /// searches the whole file, and identical locator text at another site
    /// can resolve a confidently wrong type — an anchored miss degrades to
    /// Unknown instead (see the sidecar's `matchByText` window/proximity
    /// selection, pinned by `infer-expression-line-anchor.test.ts`).
    #[test]
    fn pubsub_publisher_locator_defaults_expression_line_to_op_line() {
        use crate::operation::PubsubRole;
        use crate::services::type_sidecar::InferKind;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "dispatch/src/dispatch.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![crate::agents::file_analyzer_agent::PubsubOperation {
                    topic: "itemArchived".to_string(),
                    role: Some(PubsubRole::Publisher),
                    line_number: 9,
                    primary_type_symbol: None,
                    type_import_source: None,
                    broker: None,
                    payload_expression_text: Some("event".to_string()),
                    payload_expression_line: None,
                    backfilled: false,
                }],
                ..Default::default()
            },
        );

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_pubsub_infer_requests(&file_results, ".");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.infer_kind, InferKind::Expression);
        assert_eq!(
            request.expression_line,
            Some(9),
            "a missing payload_expression_line must default to the op's line, \
             not fall through to an unanchored file-wide search"
        );
    }

    /// PR-4: a subscriber pub/sub op carrying a decoded-payload
    /// `primary_type_symbol` reaches BOTH `cloud_data.endpoints` (via
    /// `append_pubsub_operations`, PR-3) AND the type manifest as a
    /// `ManifestRole::Producer` entry anchored on that symbol (via
    /// `append_pubsub_manifest_entries`, PR-4); a publisher op becomes a
    /// `ManifestRole::Consumer` manifest entry. This is the Socket.IO manifest
    /// path mirrored for pub/sub, so the anchor + resolution dimensions stop
    /// treating extracted pub/sub ops as untyped misses.
    #[test]
    fn pubsub_ops_reach_cloud_data_and_manifest_with_payload_anchor() {
        use crate::operation::PubsubRole;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "metrics-service/src/consumer.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "metrics.page_view",
                    PubsubRole::Subscriber,
                    Some("PageView"),
                    Some("./types/page-view"),
                )],
                ..Default::default()
            },
        );
        file_results.insert(
            "web-frontend/src/track.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "metrics.page_view",
                    PubsubRole::Publisher,
                    Some("PageView"),
                    Some("./types/page-view"),
                )],
                ..Default::default()
            },
        );

        // PR-3 side: the subscriber lands in endpoints (producer), the publisher
        // in calls (consumer), keyed identically so they match cross-repo.
        let mut cloud_data = repo_with_bundle("metrics-monorepo", None, "");
        let to_details = |key: OperationKey, file_path: &Path, line: u32| ApiEndpointDetails {
            view_module: false,
            owner: None,
            key,
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(format!("{}:{}", file_path.display(), line)),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: None,
            schema_binding: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
        };
        append_pubsub_operations(
            &mut cloud_data,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            &to_details,
            &to_details,
        );
        assert_eq!(
            cloud_data
                .endpoints
                .iter()
                .filter(|e| e.key.canonical() == "pubsub|metrics.page_view")
                .count(),
            1,
            "the subscriber must register as a producer endpoint"
        );
        assert_eq!(
            cloud_data
                .calls
                .iter()
                .filter(|c| c.key.canonical() == "pubsub|metrics.page_view")
                .count(),
            1,
            "the publisher must register as a consumer call"
        );

        // PR-4 side: both ops emit a manifest entry anchored on the payload type.
        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );

        let producer = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "pubsub|metrics.page_view" && e.role == ManifestRole::Producer
            })
            .expect("subscriber pub/sub op must emit a Producer manifest entry");
        assert_eq!(producer.type_kind, ManifestTypeKind::Response);
        assert_eq!(producer.primary_type_symbol.as_deref(), Some("PageView"));
        assert_eq!(producer.type_state, ManifestTypeState::Unknown);
        assert!(
            !producer.type_alias.is_empty(),
            "pub/sub op must get a stable type_alias"
        );

        let consumer = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "pubsub|metrics.page_view" && e.role == ManifestRole::Consumer
            })
            .expect("publisher pub/sub op must emit a Consumer manifest entry");
        assert_eq!(consumer.primary_type_symbol.as_deref(), Some("PageView"));

        // Exactly one entry per op — no phantom Request alias, mirroring socket.
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.key.canonical() == "pubsub|metrics.page_view")
                .count(),
            2
        );
    }

    /// Regression (xrepo-corpus-1): the file-analyzer double-classifies a single
    /// `socket.emit("payment:settled", …)` site as BOTH a deterministic socket
    /// op AND an LLM pub/sub op, so the emit is indexed twice — once
    /// `socket|SERVER->CLIENT|payment:settled` (correct, ground truth), once
    /// `pubsub|payment:settled` (spurious) — inflating the call set. The
    /// same-file socket-twin fold drops the pub/sub form. A REAL pub/sub op in a
    /// file with no socket twin (`orders.created`) is untouched, proving the
    /// fold keys on the same-file coincidence and not on the topic string or a
    /// broker name. The socket file uses a `./`-prefixed path to exercise the
    /// component-wise normalization against the repo-relative file_results key.
    #[test]
    fn pubsub_op_folded_when_same_file_socket_twin_present() {
        use crate::operation::{PubsubRole, SocketDirection};

        let emit_file = "payments-svc/realtime/server.ts";
        let publish_file = "payments-svc/events/orders.ts";

        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction::default(),
            sockets: crate::socket_io::SocketExtraction {
                listeners: vec![],
                emitters: vec![crate::socket_io::SocketOp {
                    key: OperationKey::socket("payment:settled", SocketDirection::ServerToClient),
                    // `./`-prefixed to prove the walk-path vs file_results-key join.
                    file_path: PathBuf::from(format!("./{emit_file}")),
                    line: 28,
                    payload_type_symbol: Some("Payment".to_string()),
                    payload_type_source: Some("../src/types".to_string()),
                }],
            },
            library: Default::default(),
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        // The spurious twin: same file + same event name as the socket emit.
        file_results.insert(
            emit_file.to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "payment:settled",
                    PubsubRole::Publisher,
                    Some("Payment"),
                    Some("../src/types"),
                )],
                ..Default::default()
            },
        );
        // A genuine pub/sub publish in a DIFFERENT file with no socket twin —
        // must survive the fold untouched.
        file_results.insert(
            publish_file.to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "orders.created",
                    PubsubRole::Publisher,
                    Some("OrderCreated"),
                    Some("./types/order"),
                )],
                ..Default::default()
            },
        );

        let mut cloud_data = repo_with_bundle("payments-svc", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &file_results,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            ".",
            &Config::default(),
        );

        // The socket emit is indexed exactly once, as the socket op.
        assert_eq!(
            cloud_data
                .calls
                .iter()
                .filter(|c| c.key.canonical() == "socket|SERVER->CLIENT|payment:settled")
                .count(),
            1,
            "the deterministic socket emitter must be indexed as a call"
        );
        // The spurious pub/sub twin was folded away (pre-fix: this is 1).
        assert_eq!(
            cloud_data
                .calls
                .iter()
                .filter(|c| c.key.canonical() == "pubsub|payment:settled")
                .count(),
            0,
            "the same-file socket-twin pub/sub op must be folded, not double-indexed"
        );
        // The unrelated real pub/sub publish is untouched.
        assert_eq!(
            cloud_data
                .calls
                .iter()
                .filter(|c| c.key.canonical() == "pubsub|orders.created")
                .count(),
            1,
            "a real pub/sub op with no same-file socket twin must survive"
        );

        // Manifest side folds identically: no orphan anchor for the folded op,
        // the real pub/sub op still anchors.
        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &extractions.sockets,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.key.canonical() == "pubsub|payment:settled")
                .count(),
            0,
            "folded pub/sub op must leave no orphan manifest anchor"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.key.canonical() == "pubsub|orders.created")
                .count(),
            1,
            "the real pub/sub op must still anchor a manifest entry"
        );
    }

    /// carrick#676: an in-process EventEmitter subscription is a contract
    /// PRODUCER on the pub/sub channel and an emission is the CONSUMER, so a
    /// census question like "what subscribes to this notification" has a row to
    /// answer with. Where the file-analyzer already reported the same site, its
    /// row wins and the deterministic one stands down: one call site is indexed
    /// once, and by the row that carries a payload anchor.
    #[test]
    fn event_bus_ops_are_indexed_and_defer_to_the_llm_row() {
        use crate::operation::PubsubRole;

        let subscribe_file = "notify-svc/src/handle-socket.ts";
        let emit_file = "worker-svc/src/event-bus.ts";

        let bus_op = |event: &str, file: &str, line: u32| crate::event_emitter::BusOp {
            key: OperationKey::pubsub(event),
            event: event.to_string(),
            file_path: PathBuf::from(file),
            line,
        };
        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction {
                subscribers: vec![bus_op("workerNotification", subscribe_file, 180)],
                publishers: vec![bus_op("workerNotification", emit_file, 389)],
            },
            ..Default::default()
        };

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        // The file-analyzer's view of the SAME emission: same file, same topic,
        // same role, one channel. Its row carries a payload anchor, so it is
        // the one to keep — the deterministic publisher must stand down.
        file_results.insert(
            emit_file.to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "workerNotification",
                    PubsubRole::Publisher,
                    Some("WorkerNotification"),
                    Some("./types/notification"),
                )],
                ..Default::default()
            },
        );
        // A broker topic in a file the deterministic pass found nothing in.
        file_results.insert(
            "worker-svc/src/queue.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "orders.created",
                    PubsubRole::Publisher,
                    None,
                    None,
                )],
                ..Default::default()
            },
        );

        let mut cloud_data = repo_with_bundle("notify-monorepo", None, "");
        append_deterministic_protocol_operations(
            &mut cloud_data,
            &extractions,
            &file_results,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            ".",
            &Config::default(),
        );

        // The subscription is the gap #676 was filed for: nothing else reports
        // it, so the deterministic row is the only one, at the AST's own line.
        let subscriber_rows: Vec<&ApiEndpointDetails> = cloud_data
            .endpoints
            .iter()
            .filter(|e| e.key.canonical() == "pubsub|workerNotification")
            .collect();
        assert_eq!(subscriber_rows.len(), 1);
        assert_eq!(
            subscriber_rows[0].file_path,
            PathBuf::from(format!("{subscribe_file}:180")),
            "the row must carry the AST's own line"
        );
        // The emission is indexed once, by the model's row (line 4, not the
        // AST's 389) — a second row for one call site would double the key.
        let publisher_rows: Vec<&ApiEndpointDetails> = cloud_data
            .calls
            .iter()
            .filter(|c| c.key.canonical() == "pubsub|workerNotification")
            .collect();
        assert_eq!(
            publisher_rows.len(),
            1,
            "one call site must produce one row, not one per pass"
        );
        assert_eq!(
            publisher_rows[0].file_path,
            PathBuf::from(format!("{emit_file}:14")),
            "the model's row is the one kept: it carries the payload anchor"
        );
        assert_eq!(
            cloud_data
                .calls
                .iter()
                .filter(|c| c.key.canonical() == "pubsub|orders.created")
                .count(),
            1,
            "a broker topic the deterministic pass cannot see must survive"
        );

        // The kept model row still anchors its manifest entry, so deferring to
        // it loses no type resolution.
        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &extractions.sockets,
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.key.canonical() == "pubsub|workerNotification")
                .count(),
            1,
            "the model's op must keep its anchor"
        );
    }

    /// A pub/sub op with no decoded payload type (`primary_type_symbol: None`)
    /// still gets a manifest entry, just with a `None` symbol — exactly how a
    /// socket emitter whose payload the extractor couldn't capture is handled.
    /// An op with no role anchors nothing and emits no entry.
    #[test]
    fn pubsub_op_without_payload_symbol_still_anchors_and_no_role_skips() {
        use crate::operation::PubsubRole;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "svc/src/handlers.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![
                    pubsub_op("orders.created", PubsubRole::Subscriber, None, None),
                    crate::agents::file_analyzer_agent::PubsubOperation {
                        topic: "orders.shipped".to_string(),
                        role: None,
                        line_number: 9,
                        primary_type_symbol: Some("Shipment".to_string()),
                        type_import_source: None,
                        broker: None,
                        payload_expression_text: None,
                        payload_expression_line: None,
                        backfilled: false,
                    },
                ],
                ..Default::default()
            },
        );

        let mut entries = Vec::new();
        append_pubsub_manifest_entries(
            &mut entries,
            &file_results,
            &crate::socket_io::SocketExtraction::default(),
            &crate::in_process_pubsub::InProcessPubsub::default(),
            &LibrarySiteIndex::default(),
            ".",
        );

        let untyped = entries
            .iter()
            .find(|e| e.key.canonical() == "pubsub|orders.created")
            .expect("untyped subscriber still emits a manifest entry");
        assert_eq!(untyped.role, ManifestRole::Producer);
        assert_eq!(untyped.primary_type_symbol, None);

        // The roleless op was dropped — no entry, regardless of its symbol.
        assert!(
            !entries
                .iter()
                .any(|e| e.key.canonical() == "pubsub|orders.shipped"),
            "a pub/sub op with no role must emit no manifest entry"
        );
    }

    /// Same fragile contract for GraphQL consumers: the alias on the consumer
    /// manifest entry (EDIT 2, `append_protocol_manifest_entries`) MUST byte-match
    /// the alias on the `collect_graphql_type_requests` SymbolRequest (EDIT 3),
    /// both `build_manifest_type_alias(key, Consumer, Response)`. If they diverge
    /// the resolved `.d.ts` never joins back and the entry stays `Unknown`.
    #[test]
    fn graphql_symbol_request_alias_matches_manifest_alias() {
        let consumer = graphql_consumer_op("order", Some("OrderView"), Some("./types"));
        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction {
                producers: vec![],
                consumers: vec![consumer.clone()],
                input_declarations: Default::default(),
            },
            sockets: crate::socket_io::SocketExtraction::default(),
            library: Default::default(),
        };

        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());
        let manifest_entry = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "graphql|query|order" && e.role == ManifestRole::Consumer
            })
            .expect("graphql consumer manifest entry");
        // EDIT 2: the consumer entry's anchor is the call-site payload symbol.
        assert_eq!(
            manifest_entry.primary_type_symbol.as_deref(),
            Some("OrderView"),
            "consumer entry must carry the request<T> anchor, not the SDL None"
        );
        let manifest_alias = manifest_entry.type_alias.clone();

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests = orchestrator.collect_graphql_type_requests(
            &extractions.graphql,
            ".",
            &modules_without_config(),
        );
        let request = requests
            .iter()
            .find(|r| r.symbol_name == "OrderView")
            .expect("graphql SymbolRequest");

        assert_eq!(
            request.alias.as_deref(),
            Some(manifest_alias.as_str()),
            "SymbolRequest.alias must byte-match the manifest entry's alias \
             (both build_manifest_type_alias(key, Consumer, Response)) or the \
             enrich-join silently breaks"
        );
        // Independently confirm both equal the canonical builder output.
        let expected = crate::type_manifest::build_manifest_type_alias(
            &consumer.key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
        );
        assert_eq!(manifest_alias, expected);
        assert_eq!(request.alias.as_deref(), Some(expected.as_str()));
    }

    /// #268: `collect_graphql_type_requests` falls back to
    /// `consumer_located_type_symbol` ONLY when `payload_type_symbol` is
    /// absent — the deterministic call-site anchor always wins when both are
    /// present (pinning the orchestrator's own precedence independently of
    /// the engine merge's isolation guard, which already keeps the two
    /// mutually exclusive per op in practice).
    #[test]
    fn collect_graphql_type_requests_falls_back_to_located_type_only_when_unanchored() {
        use crate::operation::GraphqlOperationKind;

        // No call-site anchor, but a located type — the fallback must fire.
        let mut located_only = graphql_consumer_op_at(
            GraphqlOperationKind::Subscription,
            "orderUpdated",
            "web-frontend/lib/graphql.ts",
            None,
        );
        located_only.consumer_located_type_symbol = Some("OrderUpdate".to_string());

        // BOTH a call-site anchor and a (stray) located type — the explicit
        // anchor must win, exactly mirroring the resolver-first precedent.
        let mut both = graphql_consumer_op("order", Some("OrderView"), Some("./types"));
        both.consumer_located_type_symbol = Some("StrayType".to_string());

        // Neither anchor — no request at all.
        let neither = graphql_consumer_op_at(
            GraphqlOperationKind::Query,
            "unanchored",
            "web-frontend/lib/graphql.ts",
            None,
        );

        let extraction = crate::graphql::GraphqlExtraction {
            producers: vec![],
            consumers: vec![located_only, both, neither],
            input_declarations: Default::default(),
        };

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let requests =
            orchestrator.collect_graphql_type_requests(&extraction, ".", &modules_without_config());

        assert!(
            requests.iter().any(|r| r.symbol_name == "OrderUpdate"),
            "the located type must bundle when there is no call-site anchor, got: {:?}",
            requests
        );
        assert!(
            requests.iter().any(|r| r.symbol_name == "OrderView"),
            "the call-site anchor must still bundle, got: {:?}",
            requests
        );
        assert!(
            !requests.iter().any(|r| r.symbol_name == "StrayType"),
            "a stray located type on an already-anchored op must NEVER bundle, got: {:?}",
            requests
        );
        assert_eq!(
            requests.len(),
            2,
            "exactly two requests (OrderUpdate + OrderView) — the fully-unanchored \
             op produces none, got: {:?}",
            requests
        );
    }

    /// Stage B1 producer infer join: a GraphQL PRODUCER whose resolver location
    /// was merged in (`resolver_file`/`resolver_line`) becomes a `FunctionReturn`
    /// infer request whose alias byte-matches the PRODUCER manifest entry's
    /// `type_alias` — both `build_manifest_type_alias(key, Producer, Response)`.
    /// This is the load-bearing join: if they diverge, the inferred expanded
    /// resolver-return `.d.ts` never lands on the producer entry and it stays
    /// `Unknown`. Mirrors `graphql_symbol_request_alias_matches_manifest_alias`,
    /// but on the producer/infer side.
    #[test]
    fn graphql_producer_infer_request_alias_matches_manifest_alias() {
        use crate::operation::GraphqlOperationKind;

        // A producer with its SDL anchor AND a resolver location (post-merge).
        let mut producer = graphql_op(GraphqlOperationKind::Query, "order", Some("Order"));
        producer.resolver_file = Some(PathBuf::from("packages/gateway/src/orders.resolver.ts"));
        producer.resolver_line = Some(38);

        let extractions = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction {
                producers: vec![producer.clone()],
                consumers: vec![],
                input_declarations: Default::default(),
            },
            sockets: crate::socket_io::SocketExtraction::default(),
            library: Default::default(),
        };

        // The producer manifest entry's alias (Producer, Response).
        let mut entries = Vec::new();
        append_protocol_manifest_entries(&mut entries, &extractions, &LibrarySiteIndex::default());
        let manifest_entry = entries
            .iter()
            .find(|e| {
                e.key.canonical() == "graphql|query|order" && e.role == ManifestRole::Producer
            })
            .expect("graphql producer manifest entry");
        let manifest_alias = manifest_entry.type_alias.clone();

        let orchestrator = FileOrchestrator::new(AgentService::new());
        let infer = orchestrator.collect_graphql_producer_infer_requests(
            &extractions.graphql,
            ".",
            &modules_without_config(),
        );
        assert_eq!(infer.len(), 1, "exactly one producer infer request");
        let request = &infer[0];

        // The load-bearing alias join.
        assert_eq!(
            request.alias.as_deref(),
            Some(manifest_alias.as_str()),
            "InferRequestItem.alias must byte-match the producer manifest entry's \
             alias (both build_manifest_type_alias(key, Producer, Response)) or the \
             expanded resolver-return type never joins back and the entry stays Unknown"
        );
        // Independently confirm both equal the canonical builder output.
        let expected = crate::type_manifest::build_manifest_type_alias(
            &producer.key,
            ManifestRole::Producer,
            ManifestTypeKind::Response,
        );
        assert_eq!(manifest_alias, expected);
        assert_eq!(request.alias.as_deref(), Some(expected.as_str()));

        // The producer takes the INFER path (FunctionReturn at the resolver), not
        // the bundle path: file/line come from the merged resolver location.
        assert_eq!(request.infer_kind, InferKind::FunctionReturn);
        assert_eq!(request.line_number, 38);
        assert!(
            request
                .file_path
                .ends_with("packages/gateway/src/orders.resolver.ts"),
            "infer file must be the resolver file, got: {}",
            request.file_path
        );

        // A producer without a merged resolver location yields no infer request.
        let bare = ProtocolExtractions {
            event_bus: crate::event_emitter::BusExtraction::default(),
            graphql: crate::graphql::GraphqlExtraction {
                producers: vec![graphql_op(
                    GraphqlOperationKind::Query,
                    "order",
                    Some("Order"),
                )],
                consumers: vec![],
                input_declarations: Default::default(),
            },
            sockets: crate::socket_io::SocketExtraction::default(),
            library: Default::default(),
        };
        assert!(
            orchestrator
                .collect_graphql_producer_infer_requests(
                    &bare.graphql,
                    ".",
                    &modules_without_config()
                )
                .is_empty(),
            "an SDL producer with no merged resolver location produces no infer request"
        );
    }

    /// Minimal `CloudRepoData` carrying just a repo/service identity and a
    /// bundled `.d.ts`, for the bundle-file-emission tests below.
    /// `service`'s stored or scanned copy, by `version`, stating `GET` on
    /// each of `paths`.
    fn service_stating(service: &str, version: Option<&str>, paths: &[&str]) -> CloudRepoData {
        let mut repo = repo_with_bundle("api", Some(service), "");
        repo.scanner_version = version.map(str::to_string);
        repo.endpoints = paths
            .iter()
            .map(|path| ApiEndpointDetails {
                view_module: false,
                owner: None,
                key: OperationKey::http("GET", path.to_string()),
                params: vec![],
                request_body: None,
                response_body: None,
                handler_name: None,
                request_type: None,
                response_type: None,
                file_path: PathBuf::from("src/routes.ts:1"),
                repo_name: None,
                service_name: None,
                provenance: Default::default(),
                resolution_source: None,
                dispatch: None,
                schema_binding: None,
                handler_span: None,
                name_scope: None,
                library_semantics: Vec::new(),
            })
            .collect();
        repo
    }

    /// carrick#1712: a PR's endpoint delta compares with main's index only
    /// when this scanner version wrote every stored service of the repo.
    /// Another version, or none named, gives no delta, so a row the PR did
    /// not add never shows as new or removed; no index gives none either.
    #[test]
    fn the_pr_delta_compares_only_with_an_index_this_version_wrote() {
        const THIS: &str = "0.4.0";
        let same = service_stating("web", Some(THIS), &["/orders"]);
        assert_eq!(
            DeltaBaseline::of(std::iter::empty(), THIS),
            DeltaBaseline::Absent
        );
        assert_eq!(
            DeltaBaseline::of(
                [&service_stating("web", Some("0.3.99"), &["/orders"])],
                THIS
            ),
            DeltaBaseline::OtherScanner(vec!["0.3.99".to_string()])
        );
        assert_eq!(
            DeltaBaseline::of([&service_stating("web", None, &["/orders"])], THIS),
            DeltaBaseline::OtherScanner(vec!["of unknown version".to_string()])
        );
        assert_eq!(
            DeltaBaseline::of([&same, &service_stating("jobs", Some("0.3.99"), &[])], THIS),
            DeltaBaseline::OtherScanner(vec!["0.3.99".to_string(), THIS.to_string()]),
            "one service by another version is enough"
        );
        let DeltaBaseline::Keys(previous) = DeltaBaseline::of([&same], THIS) else {
            panic!("this version's index is the baseline");
        };
        let delta = endpoint_delta(
            &previous,
            &[service_stating("web", Some(THIS), &["/orders", "/users"])],
        );
        let paths = |refs: &[crate::findings::EndpointRef]| -> Vec<String> {
            refs.iter().map(|r| r.path.clone()).collect()
        };
        assert_eq!(paths(&delta.new_endpoints), vec!["/users"]);
        assert!(delta.removed_endpoints.is_empty());
        let dropped = endpoint_delta(&previous, &[service_stating("web", Some(THIS), &[])]);
        assert_eq!(paths(&dropped.removed_endpoints), vec!["/orders"]);
        assert!(dropped.new_endpoints.is_empty());
    }

    fn repo_with_bundle(
        repo_name: &str,
        service_name: Option<&str>,
        bundled_types: &str,
    ) -> CloudRepoData {
        CloudRepoData {
            repo_name: repo_name.to_string(),
            service_name: service_name.map(str::to_string),
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test-hash".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: Some(bundled_types.to_string()),
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
        }
    }
}
