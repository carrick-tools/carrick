//! Run-level record of analysis the scan was supposed to do and did not.
//!
//! A file whose analyzer call fails after its retries are spent keeps only
//! what the deterministic layer stated about it: everything that needed the
//! model — the calls and routes only extraction reaches, and the matched rows
//! on the other side of them — is missing from the index. The scan itself does
//! not notice: the fold that collects per-file results counts the failure and
//! carries on, which is how a run could report success while silently removing
//! most of a service's endpoints (#461).
//!
//! This module is where those losses are counted, so the end of the run can
//! state them and refuse to call itself a success. It is a process-global
//! because a scan is one process, several independently-constructed services
//! analyse inside it,
//! and the question "did this run lose anything" is about the run, not about
//! any one of them.
//!
//! The type layer is counted here for the same reason: a sidecar that never
//! becomes ready costs every endpoint its request and response types while
//! leaving the route surface intact, so the run looks like a success from
//! every angle except a gate that asks for a type (carrick#748).
//!
//! What belongs here is loss the scan cannot account for: a call the cloud
//! never answered, a sidecar that missed its budget on this machine.
//! Deterministic exclusions do not — a file that fails to parse is a known,
//! repeatable limitation, and putting it here would make a repo with one
//! unparseable file permanently red. The same line divides an environmental
//! sidecar failure from a repeatable one; see
//! [`crate::services::type_sidecar::SidecarError::is_environmental`].
//!
//! A file the model was deliberately not asked about is the other side of
//! that line, and it is counted separately (carrick#555). When a budget says
//! no, the cloud answers `llm_disabled` and every call fails individually;
//! recording those as lost files made the run fail before the upload, so an
//! organisation past its allowance got no index at all — the opposite of the
//! ruling, which is that the scan completes facts-only and says so. The
//! categories and their codes are pinned in carrick-cloud
//! `docs/internal/reference/laptop-scan-seam.md` C1.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Set this to let a run finish green despite losing files. The loss is still
/// reported; only the verdict changes.
pub const ALLOW_PARTIAL_ENV: &str = "CARRICK_ALLOW_PARTIAL_ANALYSIS";

/// Set this to let a run continue with no type layer. The loss is still
/// reported; only the verdict changes.
///
/// Separate from [`ALLOW_PARTIAL_ENV`] because the two losses are different
/// and stop at different points: a lost file aborts before the upload, so the
/// existing index is not thinned; a sidecar that never becomes ready aborts
/// before the analysis, because nothing after that point can recover the
/// types and the whole LLM spend would buy a typeless index (carrick#748).
pub const ALLOW_MISSING_TYPES_ENV: &str = "CARRICK_ALLOW_MISSING_TYPES";

/// Set this (the cloud runner does) to stop the run at the first type-checker
/// memory death, so it can be run again on a larger machine (carrick#2202).
/// Unset, a memory death costs the service its types and the run goes on.
pub const FAIL_ON_TYPE_MEMORY_ENV: &str = "CARRICK_FAIL_ON_TYPE_MEMORY";

/// How many lost files are named individually before the list is truncated.
const MAX_NAMED_FILES: usize = 10;

/// What the run says about a refusal the cloud sent no sentence with.
///
/// Only ever a fallback. The cloud knows which budget refused and when
/// inference resumes; the scanner does not, and the refusal's `details.reason`
/// is an open set it deliberately never reads, so a refusal whose sentence is
/// missing is the only case this describes in the scanner's own words.
const REFUSAL_WITHOUT_A_SENTENCE: &str = concat!(
    "This workspace is past an inference allowance. They keep the rows the ",
    "deterministic layer stated and their candidates were not refreshed this run."
);

/// The counters themselves, owned rather than reached through the global.
///
/// Every rule about what a run lost lives on this value, so it can be built,
/// filled and read in isolation. The process-global below is one instance of
/// it, held for the duration of a scan; nothing about the policy needs the
/// global, which is why the tests never touch it (carrick#683).
#[derive(Default)]
struct Registry {
    /// Files dispatched to the analyzer across every service in this run.
    attempted: usize,
    /// One entry per file the analyzer never answered for.
    lost: Vec<LostFile>,
    /// Function intents that failed after their retries, per service: how
    /// many, and how many of those the model refused for capacity. Not a
    /// reason to hold a service back (a missing intent never was), only a
    /// reason to retry it before the run ends.
    intents_failed: Vec<(Scope, usize, usize)>,
    /// One entry per file the model was deliberately not asked about, because
    /// a budget refused the call. Not a loss the scan can fix by re-running,
    /// and not a reason to fail: the file keeps its deterministic rows and its
    /// candidates are simply not refreshed this run (carrick#555).
    not_refreshed: Vec<String>,
    /// The sentence the cloud refused with, from the first refusal of the run
    /// that carried one. Every refusal in a run carries the same words, so the
    /// summary prints them once however many files were refused
    /// (carrick#1413).
    refusal_sentence: Option<String>,
    /// One entry per scope that got no type layer: (scope, reason). The scope
    /// is a service name, or [`WHOLE_SCAN`] when the sidecar never became
    /// ready at all and every service in the run is typeless.
    types_unavailable: Vec<(String, String)>,
    /// Pub/sub rows the model reported and the scan left out because their
    /// events never leave the service (carrick#1513), per service. Not a loss:
    /// a count a reader needs to know the rows were dropped on purpose. A
    /// service analysed twice in one run keeps its last count.
    in_process_pubsub: BTreeMap<Scope, usize>,
    /// The library packages each service's scan ended without, after the wait
    /// for the store's builds (carrick#2496): pending at the last answer, or
    /// still building when the wait ran out. A service that asked the store
    /// and ended without nothing keeps an empty set, so "asked, none left"
    /// reads differently from "never asked". Not a loss: the file reader's
    /// rows stand, the library rows are missing and are asked again next scan.
    library_pending: BTreeMap<Scope, LibraryUnready>,
    /// What the time limits that ran out while each service was analysed
    /// cost it, by kind (carrick#2021). The cross-service check's are read
    /// off its outcomes instead ([`crate::time_limits::from_check_outcomes`]),
    /// because no service is being analysed while it runs.
    time_limits: BTreeMap<Scope, crate::time_limits::TimeLimitsRunOut>,
    /// Every type-checker process that died of memory in this run, in order
    /// (carrick#2208). With [`FAIL_ON_TYPE_MEMORY_ENV`] the first one ends
    /// the run.
    type_memory_deaths: Vec<TypeMemoryDeath>,
}

/// A type-checker process that died of memory (carrick#2208).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeMemoryDeath {
    /// The service whose analysis was running, when one was and it is named;
    /// `None` in the cross-service check, or for an unnamed service.
    pub service: Option<String>,
    /// The stage the run was in ([`crate::scan_stage::current`]).
    pub stage: crate::scan_stage::Stage,
    pub kind: crate::services::type_sidecar::MemoryDeath,
    /// The heap cap the process was started with, in MB; `None` for V8's
    /// default.
    pub heap_cap_mb: Option<u64>,
    /// It died while a service's analysis was running: `service` names that
    /// service (or is `None` for an unnamed one). `false` in the
    /// cross-service check and anywhere else after the loop.
    pub in_analysis: bool,
    /// Another service's analysis was in flight as it died (carrick#3084).
    pub beside: bool,
}

impl TypeMemoryDeath {
    /// Whether this death stops a run told to stop on one: a death outside
    /// any service's analysis always does; one in a service's analysis only
    /// when the service ran alone at the run's cap (carrick#3084). Any other
    /// is the service's to analyse again, alone, at the run's cap.
    pub fn stops_the_run(&self, run_cap_mb: Option<u64>) -> bool {
        !self.in_analysis || (!self.beside && self.heap_cap_mb == run_cap_mb)
    }
}

/// The run stopped at a type-checker memory death, because it was asked to
/// with [`FAIL_ON_TYPE_MEMORY_ENV`]: the machine is too small for this repo's
/// types, and the run is to be asked again on a larger one. `main` exits
/// with [`crate::shutdown::TYPES_OUT_OF_MEMORY_EXIT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesOutOfMemory(pub TypeMemoryDeath);

impl std::fmt::Display for TypesOutOfMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::services::type_sidecar::MemoryDeath;
        let death = &self.0;
        let how = match death.kind {
            MemoryDeath::HeapLimit => "reached its heap limit",
            MemoryDeath::OomKilled => "was killed by the kernel for memory",
        };
        let cap = match death.heap_cap_mb {
            Some(mb) => format!("{mb} MB"),
            None => "V8's default".to_string(),
        };
        let service = match &death.service {
            Some(service) => format!(" for service {service}"),
            None => String::new(),
        };
        write!(
            f,
            "types_out_of_memory: the type checker {how} ({}) during {}{service}, under a heap \
             cap of {cap}; the run stopped so it can run again on a larger machine",
            death.kind.as_str(),
            death.stage.as_str(),
        )
    }
}

impl std::error::Error for TypesOutOfMemory {}

/// Whether the run stops at a memory death: when `fail_on_memory` is set, at
/// the first recorded that [`TypeMemoryDeath::stops_the_run`] under
/// `run_cap_mb`. A death beside another service, or under a cap below the
/// run's, does not stop it: that service is analysed again alone
/// (carrick#3084). Pure, so the policy is testable without the process
/// environment.
pub fn stop_for_type_memory(
    fail_on_memory: bool,
    deaths: &[TypeMemoryDeath],
    run_cap_mb: Option<u64>,
) -> Result<(), TypesOutOfMemory> {
    if !fail_on_memory {
        return Ok(());
    }
    match deaths.iter().find(|death| death.stops_the_run(run_cap_mb)) {
        Some(first) => Err(TypesOutOfMemory(first.clone())),
        None => Ok(()),
    }
}

/// Which service a loss belongs to: its `service_name`, or `None` for a repo
/// scanned as one unnamed service. The same key `CloudRepoData::service_name`
/// carries, so a payload finds its own losses without a label mapping.
type Scope = Option<String>;

/// One file the analyzer never answered for.
struct LostFile {
    scope: Scope,
    path: String,
    reason: LossReason,
}

/// Why a file has no analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossReason {
    /// A stable code, not a sentence: the cloud's error code, or a
    /// scanner-side pseudo-code. The summary groups by it and the upload
    /// names it.
    pub code: String,
    /// The cloud answered that the request cannot succeed
    /// ([`crate::agent_service::AgentCallError::is_final_answer`]). Asking
    /// again gets the same answer, so the file is not asked about again in
    /// this run and nobody is told that running again finishes it
    /// (carrick#1897).
    pub is_final: bool,
}

impl LossReason {
    /// A loss another ask may mend: the file is owed, as every loss was
    /// before the cloud kept a final answer.
    pub fn owed(code: &str) -> Self {
        Self {
            code: code.to_string(),
            is_final: false,
        }
    }
}

/// What one service still owes the model at a point in the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceLosses {
    /// Files dispatched to the analyzer that got no answer.
    pub files: usize,
    /// Function intents that failed after their retries.
    pub intents: usize,
    /// How many of `files` are owed for capacity
    /// ([`crate::agent_service::is_capacity_code`]): the model refused the
    /// file's own request, or the file was not sent behind a refusal on a
    /// route that had stopped answering (carrick#1909). They, and
    /// `capacity_intents`, are the losses a wait before the retry can mend
    /// (carrick#1896).
    pub capacity_files: usize,
    /// How many of `intents` ended the same way.
    pub capacity_intents: usize,
    /// How many of `files` ended on an answer the cloud called final
    /// ([`LossReason::is_final`]). They have no analysis, like the rest, and
    /// no further ask changes that.
    pub final_files: usize,
}

impl ServiceLosses {
    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.intents == 0
    }
}

impl Registry {
    /// Adds a service's dispatched file count to the total.
    fn record_files_attempted(&mut self, count: usize) {
        self.attempted += count;
    }

    /// Records that `path` has no analysis, and why.
    fn record_unanalysed_file(&mut self, scope: &Scope, path: &str, reason: &LossReason) {
        self.lost.push(LostFile {
            scope: scope.clone(),
            path: path.to_string(),
            reason: reason.clone(),
        });
    }

    /// Records that `count` intents of `scope` failed, `for_capacity` of them
    /// because the model refused for capacity.
    fn record_intents_failed(&mut self, scope: &Scope, count: usize, for_capacity: usize) {
        if count == 0 {
            return;
        }
        match self.intents_failed.iter_mut().find(|(s, _, _)| s == scope) {
            Some((_, total, total_for_capacity)) => {
                *total += count;
                *total_for_capacity += for_capacity;
            }
            None => self
                .intents_failed
                .push((scope.clone(), count, for_capacity)),
        }
    }

    fn service_losses(&self, scope: &Scope) -> ServiceLosses {
        let files = self.lost.iter().filter(|l| l.scope == *scope);
        let intents = self.intents_failed.iter().filter(|(s, _, _)| s == scope);
        ServiceLosses {
            files: files.clone().count(),
            intents: intents.clone().map(|(_, n, _)| *n).sum(),
            capacity_files: files
                .clone()
                .filter(|l| crate::agent_service::is_capacity_code(&l.reason.code))
                .count(),
            capacity_intents: intents.map(|(_, _, for_capacity)| *for_capacity).sum(),
            final_files: files.filter(|l| l.reason.is_final).count(),
        }
    }

    /// Drop what one service lost, ahead of analysing it again.
    ///
    /// Its lost files leave `attempted` too: the retry dispatches exactly
    /// those files again and counts them again, and a file asked about twice
    /// is still one file of the run.
    fn forget_service_losses(&mut self, scope: &Scope) {
        let before = self.lost.len();
        self.lost.retain(|l| l.scope != *scope);
        self.attempted = self.attempted.saturating_sub(before - self.lost.len());
        self.intents_failed.retain(|(s, _, _)| s != scope);
        self.time_limits.remove(scope);
        self.library_pending.remove(scope);
    }

    /// Records that `limit` ran out while `scope` was analysed, at a cost of
    /// `cost` items.
    fn record_time_limit(
        &mut self,
        scope: &Scope,
        limit: crate::time_limits::TimeLimit,
        cost: usize,
    ) {
        if cost == 0 {
            return;
        }
        self.time_limits
            .entry(scope.clone())
            .or_default()
            .add(limit, cost);
    }

    /// What the time limits that ran out cost `scope` while it was analysed.
    fn time_limits_for(&self, scope: &Scope) -> crate::time_limits::TimeLimitsRunOut {
        self.time_limits.get(scope).copied().unwrap_or_default()
    }

    /// The files `scope` lost that another ask may mend.
    fn unanalysed_files_for(&self, scope: &Scope) -> Vec<crate::cloud_storage::UnanalysedFile> {
        self.lost_files_for(scope, false)
    }

    /// The files `scope` lost for good: their answer is final
    /// ([`LossReason::is_final`]), so every scan of the same bytes loses them
    /// the same way (carrick#2488).
    fn final_losses_for(&self, scope: &Scope) -> Vec<crate::cloud_storage::UnanalysedFile> {
        self.lost_files_for(scope, true)
    }

    fn lost_files_for(
        &self,
        scope: &Scope,
        is_final: bool,
    ) -> Vec<crate::cloud_storage::UnanalysedFile> {
        self.lost
            .iter()
            .filter(|l| l.scope == *scope && l.reason.is_final == is_final)
            .map(|l| crate::cloud_storage::UnanalysedFile {
                path: l.path.clone(),
                reason: l.reason.code.clone(),
            })
            .collect()
    }

    /// Records that the model was not asked about `path`, on purpose, and the
    /// sentence the cloud refused with.
    ///
    /// The first non-empty sentence wins and the rest are dropped: they are
    /// the same words repeated once per refused file.
    fn record_candidates_not_refreshed(&mut self, path: &str, sentence: Option<&str>) {
        self.not_refreshed.push(path.to_string());
        if self.refusal_sentence.is_none() {
            self.refusal_sentence = sentence
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
    }

    /// One line naming how many files the budget refused, or `None`.
    ///
    /// The cause is the cloud's own words, verbatim: which budget refused and
    /// when inference resumes are facts about the account that only the cloud
    /// holds, and the reason code beside them is an open set, so a scanner
    /// that described the cause itself would eventually describe it wrongly
    /// (carrick#1413). [`REFUSAL_WITHOUT_A_SENTENCE`] covers a refusal that
    /// carried no words.
    fn not_refreshed_line(&self) -> Option<String> {
        if self.not_refreshed.is_empty() {
            return None;
        }
        Some(format!(
            "{} of {} files were not sent to the model. {}",
            self.not_refreshed.len(),
            self.attempted,
            self.refusal_sentence
                .as_deref()
                .unwrap_or(REFUSAL_WITHOUT_A_SENTENCE),
        ))
    }

    /// Records that `scope` has no type layer this run, and why.
    ///
    /// First reason wins: one sidecar failure is seen by several stages (the
    /// per-service re-init, then the resolve step), and the summary counts
    /// scopes that lost their types, not stages that noticed.
    fn record_types_unavailable(&mut self, scope: &str, reason: &str) {
        if self.types_unavailable.iter().any(|(s, _)| s == scope) {
            return;
        }
        self.types_unavailable
            .push((scope.to_string(), reason.to_string()));
    }

    /// Why `scope` has no type layer, if it was recorded as losing one.
    fn types_unavailable_reason(&self, scope: &str) -> Option<String> {
        self.types_unavailable
            .iter()
            .find(|(recorded, _)| recorded == scope)
            .map(|(_, reason)| reason.clone())
    }

    /// One line naming what lost its types and why, or `None` when nothing
    /// did.
    ///
    /// Says what the loss costs, not just that it happened: a run that keeps
    /// every route but drops every request and response type reads as a
    /// success everywhere except the one gate that asks for a type, which is
    /// how 0.3.42 shipped a typeless index for a 33-service repo and only the
    /// type rows of an external gate noticed (carrick#748).
    fn types_summary_line(&self) -> Option<String> {
        if self.types_unavailable.is_empty() {
            return None;
        }

        let scopes = self
            .types_unavailable
            .iter()
            .map(|(scope, reason)| format!("{} ({})", scope, reason))
            .collect::<Vec<_>>()
            .join(", ");

        Some(format!(
            "no type layer for {}: this run's endpoints carry no request or response \
             types",
            scopes
        ))
    }

    /// Records how many pub/sub rows `scope` left out as in-process.
    /// Replaces the service's earlier count, so a service that is analysed
    /// again is not counted twice.
    fn record_in_process_pubsub(&mut self, scope: &Scope, count: usize) {
        self.in_process_pubsub.insert(scope.clone(), count);
    }

    /// Records the packages `scope` ended without (carrick#2496). Replaces
    /// the service's earlier sets, so a service analysed again is not counted
    /// twice.
    fn record_library_packages(&mut self, scope: &Scope, unready: LibraryUnready) {
        self.library_pending.insert(scope.clone(), unready);
    }

    /// How many packages `scope` ended without, still pending and build-failed
    /// in that order, or `None` when it never asked the store.
    fn library_packages_for(&self, scope: &Scope) -> Option<(usize, usize)> {
        self.library_pending
            .get(scope)
            .map(|unready| (unready.pending.len(), unready.failed.len()))
    }

    /// The sentences naming the packages the run ended without: first those
    /// still pending when the wait ended, then those the store could not
    /// build. Empty when there are none.
    fn library_pending_lines(&self) -> Vec<String> {
        let pending: std::collections::BTreeSet<&String> = self
            .library_pending
            .values()
            .flat_map(|unready| &unready.pending)
            .collect();
        let failed: std::collections::BTreeSet<&String> = self
            .library_pending
            .values()
            .flat_map(|unready| &unready.failed)
            .collect();
        let mut lines = Vec::new();
        if !pending.is_empty() {
            let (n, names) = (pending.len(), named_packages(&pending));
            lines.push(if n == 1 {
                format!("1 library was not ready in time, so calls through it may be missing from this index: {names}. The next scan adds it.")
            } else {
                format!("{n} libraries were not ready in time, so calls through them may be missing from this index: {names}. The next scan adds them.")
            });
        }
        if !failed.is_empty() {
            let (n, names) = (failed.len(), named_packages(&failed));
            lines.push(if n == 1 {
                format!("Carrick could not read 1 library yet, so calls through it are missing from this index: {names}.")
            } else {
                format!("Carrick could not read {n} libraries yet, so calls through them are missing from this index: {names}.")
            });
        }
        lines
    }

    /// One line saying how many pub/sub calls were left out, or `None`.
    fn in_process_pubsub_line(&self) -> Option<String> {
        let total: usize = self.in_process_pubsub.values().sum();
        (total > 0).then(|| {
            format!(
                "{total} pub/sub call(s) left out of the index: their events stay inside the service."
            )
        })
    }

    /// How many files were lost.
    fn lost_file_count(&self) -> usize {
        self.lost.len()
    }

    /// One line naming what was lost and why, or `None` when nothing was.
    ///
    /// Grouped by reason and ordered most-frequent first, because the useful
    /// fact is the cause: twelve files lost to one expired token is a different
    /// incident from twelve lost to twelve different failures.
    fn summary_line(&self) -> Option<String> {
        if self.lost.is_empty() {
            return None;
        }

        let mut by_reason: BTreeMap<&str, usize> = BTreeMap::new();
        for lost in &self.lost {
            *by_reason.entry(lost.reason.code.as_str()).or_default() += 1;
        }
        let mut reasons: Vec<(&str, usize)> = by_reason.into_iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let reasons = reasons
            .iter()
            .map(|(reason, count)| format!("{} {}", count, reason))
            .collect::<Vec<_>>()
            .join(", ");

        let mut paths: Vec<&str> = self.lost.iter().map(|l| l.path.as_str()).collect();
        paths.sort_unstable();
        let named = paths
            .iter()
            .take(MAX_NAMED_FILES)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let and_more = if paths.len() > MAX_NAMED_FILES {
            format!(" and {} more", paths.len() - MAX_NAMED_FILES)
        } else {
            String::new()
        };

        Some(format!(
            "{} of {} files were not analysed: {}. Their endpoints and calls are missing \
             from this run's results ({}{})",
            self.lost.len(),
            self.attempted,
            reasons,
            named,
            and_more
        ))
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Adds a service's dispatched file count to the run total. Called once per
/// service, since a repo can hold several.
pub fn record_files_attempted(count: usize) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_files_attempted(count);
}

/// Records that `path` has no analysis in this run's index, and why
/// ([`analysis_failure_reason`]), against the service whose analysis is
/// running ([`crate::current_service`]): the run lands, holds back and retries
/// services one at a time, and each of those decisions is about ONE service's
/// losses.
pub fn record_unanalysed_file(path: &str, reason: &LossReason) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_unanalysed_file(&crate::current_service::name(), path, reason);
}

/// Records that the model was deliberately not asked about `path`, with the
/// sentence the cloud refused with when it sent one ([`refusal_sentence`]).
///
/// Not a lost file: nothing failed, a budget said no. The run does not fail,
/// the file is not named in `unanalysed_files` on the upload, and the index
/// keeps every row the deterministic layer stated about it.
pub fn record_candidates_not_refreshed(path: &str, sentence: Option<&str>) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_candidates_not_refreshed(path, sentence);
}

/// Whether a failed analyzer call is a file this run LOST, as opposed to one
/// it was told not to ask about.
///
/// One code is not a loss: [`crate::agent_service::LLM_DISABLED_CODE`], a
/// limit of the cloud's refused the call, whatever `details.reason` says. The
/// ruling is that hitting a limit never stops a scan: it completes facts-only
/// and never fails, so counting these would hold the service back and leave
/// the organisation with no fresh index (carrick#555, carrick-cloud#401).
///
/// Everything else — the model was asked and did not answer — is a loss.
pub fn counts_as_lost_file(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<crate::agent_service::AgentCallError>()
        .is_none_or(|e| !e.is_budget_refusal())
}

/// Whether a failed analyzer call means a budget refused it.
pub fn is_budget_refusal(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<crate::agent_service::AgentCallError>()
        .is_some_and(|e| e.is_budget_refusal())
}

/// The sentence a refusal carried, when the failure is one and the cloud sent
/// words with it.
///
/// The refusal's `details.reason` stays unread here, as it does everywhere a
/// refusal is handled: it is an open set, and a reason this build has never
/// heard of must change nothing about what the scan does. The message is what
/// the person running the scan should read, so it is what the summary prints.
pub fn refusal_sentence<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a str> {
    let call = error.downcast_ref::<crate::agent_service::AgentCallError>()?;
    call.is_budget_refusal()
        .then(|| call.message.trim())
        .filter(|sentence| !sentence.is_empty())
}

/// Why a file analysis failed, for [`record_unanalysed_file`].
///
/// Reads the cloud's own error code where the failure came from a lambda call,
/// so the summary says `gateway_error` or `oidc_rejected` rather than a
/// paragraph, and whether the cloud called its answer final. Anything else is
/// a malformed answer rather than an absent one, and is owed.
pub fn analysis_failure_reason(error: &(dyn std::error::Error + 'static)) -> LossReason {
    match error.downcast_ref::<crate::agent_service::AgentCallError>() {
        Some(call) => LossReason {
            code: call.code.clone(),
            is_final: call.is_final_answer(),
        },
        None => LossReason::owed("unparseable_response"),
    }
}

/// What a run says about `files` files whose answer the cloud called final,
/// in place of advice to run again: a re-run sends the same request and is
/// answered the same. `None` when there are none.
pub fn final_answer_sentence(files: usize) -> Option<String> {
    (files > 0).then(|| {
        format!(
            "The answer for {files} file(s) is final: a re-run gives the same result until the \
             file changes or Carrick is updated."
        )
    })
}

/// How many files this run lost.
pub fn lost_file_count() -> usize {
    registry()
        .lock()
        .expect("scan health lock")
        .lost_file_count()
}

/// How many files this run sent to the analyzer.
pub fn attempted_count() -> usize {
    registry().lock().expect("scan health lock").attempted
}

/// One line naming what the run lost and why, or `None` when it lost nothing.
pub fn summary_line() -> Option<String> {
    registry().lock().expect("scan health lock").summary_line()
}

/// One line naming how many files a budget refused, or `None` when none were.
pub fn not_refreshed_line() -> Option<String> {
    registry()
        .lock()
        .expect("scan health lock")
        .not_refreshed_line()
}

/// The files ONE service lost, in the shape its upload reports them.
///
/// Only the second category: a file the model was asked about and did not
/// answer for. A file a budget refused is never here, so the cloud's
/// first-scan partial rule is never asked to accept a partial index that is
/// only partial because the organisation is out of allowance.
///
/// Scoped to the service because the cloud applies its partial rule to the
/// row the write lands on: a run-wide list made a clean service's write carry
/// a sibling's losses, and once both had rows the clean one was refused
/// `409 partial_refused` for files it never held.
///
/// Only files another ask may mend. A file whose answer is final is in
/// [`final_losses_for`] instead: it is missing from every scan of those
/// bytes, so naming it here made the cloud refuse every later write of a
/// service that already had an index (carrick#2488).
pub fn unanalysed_files_for(service: Option<&str>) -> Vec<crate::cloud_storage::UnanalysedFile> {
    registry()
        .lock()
        .expect("scan health lock")
        .unanalysed_files_for(&service.map(str::to_string))
}

/// The files ONE service lost for good, in the shape its upload reports them
/// (`final_losses` on the write, carrick#2488): the cloud records them and
/// does not count them as owed.
pub fn final_losses_for(service: Option<&str>) -> Vec<crate::cloud_storage::UnanalysedFile> {
    registry()
        .lock()
        .expect("scan health lock")
        .final_losses_for(&service.map(str::to_string))
}

/// How many package names the library-pending sentence spells out.
const LIBRARY_PENDING_NAMED: usize = 5;

/// The library packages one service's scan ended without (carrick#2496).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryUnready {
    /// Still pending when the wait ended, whatever the pending reason.
    pub pending: std::collections::BTreeSet<String>,
    /// Answered `skipped` with `build_failed`: named, never waited on.
    pub failed: std::collections::BTreeSet<String>,
}

/// At most [`LIBRARY_PENDING_NAMED`] names, then "and N more".
fn named_packages(names: &std::collections::BTreeSet<&String>) -> String {
    let shown: Vec<&str> = names
        .iter()
        .take(LIBRARY_PENDING_NAMED)
        .map(|name| name.as_str())
        .collect();
    let more = names.len() - shown.len();
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// Records the library packages the current service's scan ended without
/// (carrick#2496), empty sets when it asked the store and ended without none.
pub fn record_library_packages(unready: LibraryUnready) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_library_packages(&crate::current_service::name(), unready);
}

/// How many library packages `service` ended without (pending, build-failed),
/// or `None` when it did not ask the store.
pub fn library_packages_for(service: Option<&str>) -> Option<(usize, usize)> {
    registry()
        .lock()
        .expect("scan health lock")
        .library_packages_for(&service.map(str::to_string))
}

/// The sentences naming the library packages the run ended without.
pub fn library_pending_lines() -> Vec<String> {
    registry()
        .lock()
        .expect("scan health lock")
        .library_pending_lines()
}

/// Records how many pub/sub rows the current service left out as in-process
/// (carrick#1513).
pub fn record_in_process_pubsub(count: usize) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_in_process_pubsub(&crate::current_service::name(), count);
}

/// One line saying how many pub/sub calls the run left out as in-process, or
/// `None` when it left out none.
pub fn in_process_pubsub_line() -> Option<String> {
    registry()
        .lock()
        .expect("scan health lock")
        .in_process_pubsub_line()
}

/// Records that `count` of the current service's function intents failed
/// after their retries, `for_capacity` of them because the model refused for
/// capacity.
pub fn record_intents_failed(count: usize, for_capacity: usize) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_intents_failed(&crate::current_service::name(), count, for_capacity);
}

/// What `service` still owes the model.
pub fn service_losses(service: Option<&str>) -> ServiceLosses {
    registry()
        .lock()
        .expect("scan health lock")
        .service_losses(&service.map(str::to_string))
}

/// Forget what `service` lost, because it is about to be analysed again and
/// will record whatever it loses a second time.
pub fn forget_service_losses(service: Option<&str>) {
    registry()
        .lock()
        .expect("scan health lock")
        .forget_service_losses(&service.map(str::to_string));
}

/// Records that `limit` ran out at a cost of `cost` items, against the
/// service whose analysis is running ([`crate::current_service`],
/// carrick#2021).
///
/// Nothing is recorded when no service's analysis is running: the
/// cross-service check belongs to no one service, and its time limits are
/// read off its outcomes instead
/// ([`crate::time_limits::from_check_outcomes`]). Nothing is recorded for a
/// cost of 0 either.
pub fn record_time_limit(limit: crate::time_limits::TimeLimit, cost: usize) {
    if !crate::current_service::in_scope() {
        return;
    }
    let scope = crate::current_service::name();
    registry()
        .lock()
        .expect("scan health lock")
        .record_time_limit(&scope, limit, cost);
}

/// What the time limits that ran out cost `service` while it was analysed.
pub fn time_limits_for(service: Option<&str>) -> crate::time_limits::TimeLimitsRunOut {
    registry()
        .lock()
        .expect("scan health lock")
        .time_limits_for(&service.map(str::to_string))
}

/// Whether [`ALLOW_PARTIAL_ENV`] is set for this run.
pub fn allow_partial_from_env() -> bool {
    env_flag(ALLOW_PARTIAL_ENV)
}

/// Whether [`ALLOW_MISSING_TYPES_ENV`] is set for this run.
pub fn allow_missing_types_from_env() -> bool {
    env_flag(ALLOW_MISSING_TYPES_ENV)
}

/// Whether [`FAIL_ON_TYPE_MEMORY_ENV`] is set for this run.
pub fn fail_on_type_memory_from_env() -> bool {
    env_flag(FAIL_ON_TYPE_MEMORY_ENV)
}

/// Records that a type-checker process died of memory, against the service
/// whose analysis is running and the stage the run is in (carrick#2208).
pub fn record_type_memory_death(
    kind: crate::services::type_sidecar::MemoryDeath,
    heap_cap_mb: Option<u64>,
) {
    let in_analysis = crate::current_service::in_scope();
    let death = TypeMemoryDeath {
        service: crate::current_service::name(),
        stage: crate::scan_stage::current(),
        kind,
        heap_cap_mb,
        in_analysis,
        // The run's ledger counts this service's own analysis too.
        beside: in_analysis && crate::engine::service_workers::run_ledger().in_flight() > 1,
    };
    registry()
        .lock()
        .expect("scan health lock")
        .type_memory_deaths
        .push(death);
}

/// `Err` when this run stops at a memory death ([`stop_for_type_memory`]
/// over [`FAIL_ON_TYPE_MEMORY_ENV`] and what was recorded). Read where a
/// service's analysis ends, after the cross-service check, and by `main` for
/// the exit code.
pub fn type_memory_stop() -> Result<(), TypesOutOfMemory> {
    let fail_on_memory = fail_on_type_memory_from_env();
    let registry = registry().lock().expect("scan health lock");
    stop_for_type_memory(
        fail_on_memory,
        &registry.type_memory_deaths,
        crate::services::type_sidecar::heap_cap_mb(),
    )
}

/// Whether `service` is to be analysed again, alone, at the run's cap: the
/// run was told to stop on a memory death, and its checker died of memory
/// in a way that does not stop the run (beside another service, or under a
/// cap below the run's; carrick#3084).
pub fn to_analyse_again_alone(service: Option<&str>) -> bool {
    if !fail_on_type_memory_from_env() {
        return false;
    }
    let run_cap = crate::services::type_sidecar::heap_cap_mb();
    registry()
        .lock()
        .expect("scan health lock")
        .type_memory_deaths
        .iter()
        .any(|death| {
            death.in_analysis
                && death.service.as_deref() == service
                && !death.stops_the_run(run_cap)
        })
}

/// The first memory death recorded in `service`'s analysis, for the line
/// that says why it is analysed again.
pub fn first_type_memory_death_of(service: Option<&str>) -> Option<TypeMemoryDeath> {
    registry()
        .lock()
        .expect("scan health lock")
        .type_memory_deaths
        .iter()
        .find(|death| death.in_analysis && death.service.as_deref() == service)
        .cloned()
}

/// Forget `service`'s memory deaths and the type loss they cost it, ahead
/// of analysing it again alone (carrick#3084): whatever it loses then is
/// recorded again.
pub fn forget_type_memory_loss(service: Option<&str>) {
    let mut registry = registry().lock().expect("scan health lock");
    registry
        .type_memory_deaths
        .retain(|death| !(death.in_analysis && death.service.as_deref() == service));
    let scope = service.unwrap_or("(root)");
    registry.types_unavailable.retain(|(s, _)| s != scope);
}

/// [`type_memory_stop`] for one service's analysis: `Err` when this run
/// stops at a memory death and one was recorded while `service` was being
/// analysed. A service analysed beside it whose own checker lived is not
/// stopped by it, and lands (carrick#1066).
pub fn type_memory_stop_of(service: Option<&str>) -> Result<(), TypesOutOfMemory> {
    let fail_on_memory = fail_on_type_memory_from_env();
    let registry = registry().lock().expect("scan health lock");
    let deaths: Vec<TypeMemoryDeath> = registry
        .type_memory_deaths
        .iter()
        .filter(|death| death.service.as_deref() == service)
        .cloned()
        .collect();
    stop_for_type_memory(
        fail_on_memory,
        &deaths,
        crate::services::type_sidecar::heap_cap_mb(),
    )
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// The scope recorded when the whole run is typeless, not one service.
pub const WHOLE_SCAN: &str = "this scan";

/// Records that `scope` has no type layer in this run's index, and why.
pub fn record_types_unavailable(scope: &str, reason: &str) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_types_unavailable(scope, reason);
}

/// Why `scope` has no type layer, if it was recorded as losing one.
pub fn types_unavailable_reason(scope: &str) -> Option<String> {
    registry()
        .lock()
        .expect("scan health lock")
        .types_unavailable_reason(scope)
}

/// One line naming what lost its types and why, or `None` when nothing did.
pub fn types_summary_line() -> Option<String> {
    registry()
        .lock()
        .expect("scan health lock")
        .types_summary_line()
}

/// Whether a readiness failure must stop the run before it spends anything.
///
/// Pure, so the policy is testable without the process environment. The split
/// is between an environmental failure and a repeatable one: a sidecar that
/// missed its budget or died is a machine having a bad day, and a scan that
/// continues past it replaces a typed index with a typeless one. A sidecar
/// that is not installed, or that answered with an error, is a property of
/// this checkout — it would fail identically on every run, and making a repo
/// permanently red is the thing this module's doc comment forbids.
pub fn should_fail_on_missing_types(fatal: bool, allow_missing: bool) -> bool {
    fatal && !allow_missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_service::AgentCallError;

    /// Every test owns its counters. Nothing here reads the process-global, so
    /// two of these running at once cannot see each other's numbers — which is
    /// the race #683 fixed, and the reason there is no `reset()` to call.
    fn run() -> Registry {
        Registry::default()
    }

    /// carrick#555, the incident this split exists for. On 2026-08-23 a daily
    /// cap answered every analyzer call with `llm_disabled`; each one was
    /// recorded as a lost file, the lost-file gate then failed the run, and
    /// every scan in the installation ended with exit 1 and no index — while
    /// its own message said the scan continues.
    ///
    /// The rule is that a file a budget refused is not a file the scan lost:
    /// nothing failed, the model was not asked. It is counted on its own line,
    /// the run does not fail, and the upload proceeds facts-only.
    #[test]
    fn a_budget_refusal_is_not_a_lost_file_and_does_not_fail_the_run() {
        let refused = AgentCallError::from_cloud(
            crate::agent_service::LLM_DISABLED_CODE,
            "past the monthly allowance",
            false,
        );
        assert!(!counts_as_lost_file(&refused));
        assert!(is_budget_refusal(&refused));

        let mut registry = run();
        registry.record_files_attempted(3);
        for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
            assert!(!counts_as_lost_file(&refused));
            registry.record_candidates_not_refreshed(path, refusal_sentence(&refused));
        }

        assert_eq!(registry.lost_file_count(), 0);
        assert!(registry.summary_line().is_none(), "nothing was lost");
        assert!(registry.service_losses(&None).is_empty());

        let line = registry.not_refreshed_line().expect("the run says so");
        assert!(
            line.contains("3 of 3 files were not sent to the model"),
            "{line}"
        );
        assert!(line.contains("past the monthly allowance"), "{line}");
    }

    /// The reason the sentence is quoted rather than written here: a refusal
    /// the scanner has no vocabulary for still says the right thing. This one
    /// is refused for a cause the scanner knows nothing about, and the line
    /// reads back the cloud's words and nothing of its own.
    #[test]
    fn the_line_quotes_the_refusal_the_cloud_sent() {
        const SENT: &str = "Carrick has reached today's limit. This scan finished with facts only; \
             inferred results resume after 00:00 UTC.";
        let refused =
            AgentCallError::from_cloud(crate::agent_service::LLM_DISABLED_CODE, SENT, false);
        assert_eq!(refusal_sentence(&refused), Some(SENT));

        let mut registry = run();
        registry.record_files_attempted(4);
        registry.record_candidates_not_refreshed("src/a.ts", refusal_sentence(&refused));

        let line = registry.not_refreshed_line().expect("the run says so");
        assert_eq!(
            line,
            format!("1 of 4 files were not sent to the model. {SENT}")
        );
        assert!(
            !line.contains("inference allowance"),
            "the scanner's own guess must not follow the cloud's sentence: {line}"
        );
    }

    /// Every refused file carries the same words, so the run prints them once.
    /// The first sentence wins; nothing after it is read.
    #[test]
    fn many_refused_files_print_one_sentence() {
        let mut registry = run();
        registry.record_files_attempted(9);
        for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
            registry.record_candidates_not_refreshed(path, Some("Inference is paused."));
        }

        let line = registry.not_refreshed_line().expect("the run says so");
        assert_eq!(
            line,
            "3 of 9 files were not sent to the model. Inference is paused."
        );
        assert_eq!(
            line.matches("Inference is paused.").count(),
            1,
            "one sentence, however many files: {line}"
        );
    }

    /// A refusal with no words of its own — an older cloud, or an empty
    /// message — still gets a line, in the scanner's own fallback.
    #[test]
    fn a_refusal_that_carried_no_sentence_falls_back() {
        let wordless =
            AgentCallError::from_cloud(crate::agent_service::LLM_DISABLED_CODE, "   ", false);
        assert_eq!(refusal_sentence(&wordless), None, "blank is not a sentence");

        let mut registry = run();
        registry.record_files_attempted(2);
        registry.record_candidates_not_refreshed("src/a.ts", refusal_sentence(&wordless));
        registry.record_candidates_not_refreshed("src/b.ts", None);

        let line = registry.not_refreshed_line().expect("the run says so");
        assert_eq!(
            line,
            format!("2 of 2 files were not sent to the model. {REFUSAL_WITHOUT_A_SENTENCE}")
        );
    }

    /// Only a refusal has a sentence to quote. A failure's message is our own
    /// diagnostic text and belongs in the lost-file report, not in a line that
    /// tells somebody why their scan was facts-only.
    #[test]
    fn a_failure_that_is_not_a_refusal_has_no_sentence() {
        let failed = AgentCallError::from_cloud("model_error", "the model returned nothing", true);
        assert_eq!(refusal_sentence(&failed), None);
        assert_eq!(
            refusal_sentence(&std::io::Error::other("connection reset")),
            None
        );
    }

    /// The other half of the split, which must keep working exactly as it did:
    /// the model WAS asked and did not answer, so the file is lost, the run
    /// fails, and the upload is skipped so a thinner index cannot overwrite a
    /// good one (#461).
    #[test]
    fn a_call_that_was_made_and_failed_is_still_a_lost_file() {
        let failed = AgentCallError::from_cloud("model_error", "the model returned nothing", true);
        assert!(counts_as_lost_file(&failed));
        assert!(!is_budget_refusal(&failed));

        let mut registry = run();
        registry.record_files_attempted(2);
        registry.record_unanalysed_file(&None, "src/a.ts", &analysis_failure_reason(&failed));
        assert_eq!(registry.lost_file_count(), 1);
        assert_eq!(registry.service_losses(&None).files, 1);
        assert!(registry.not_refreshed_line().is_none());
        assert!(registry.summary_line().unwrap().contains("model_error"));
    }

    /// A `rate_limited` answer whose retries ran out is a loss like any other
    /// spent transient answer: the model was asked and did not answer. It is
    /// not a refusal, whatever the word sounds like (carrick-cloud#401).
    #[test]
    fn a_spent_rate_limited_answer_is_a_loss_not_a_refusal() {
        let spent = AgentCallError::from_cloud("rate_limited", "Gemini quota exceeded", true);
        assert!(counts_as_lost_file(&spent));
        assert!(!is_budget_refusal(&spent));
    }

    /// A failure that never produced an envelope is a loss: the scanner cannot
    /// know it was refused on purpose, so it must not assume it was.
    #[test]
    fn an_untyped_failure_counts_as_a_loss() {
        let untyped = std::io::Error::other("connection reset");
        assert!(counts_as_lost_file(&untyped));
        assert!(!is_budget_refusal(&untyped));
        assert_eq!(
            analysis_failure_reason(&untyped),
            LossReason::owed("unparseable_response")
        );
    }

    /// A run that lost nothing says nothing and passes.
    #[test]
    fn a_clean_run_has_no_summary_and_does_not_fail() {
        let mut run = run();
        run.record_files_attempted(120);
        assert_eq!(run.summary_line(), None);
        assert_eq!(run.in_process_pubsub_line(), None);
        assert!(run.service_losses(&None).is_empty());
    }

    /// carrick#1513: rows the scan left out on purpose are counted where a
    /// reader sees them, summed over services, and a service analysed twice
    /// counts once. Never a loss.
    #[test]
    fn left_out_pubsub_rows_are_counted_once_per_service() {
        let mut run = run();
        run.record_in_process_pubsub(&Some("api".to_string()), 16);
        run.record_in_process_pubsub(&Some("api".to_string()), 16);
        run.record_in_process_pubsub(&Some("worker".to_string()), 2);
        assert_eq!(
            run.in_process_pubsub_line().as_deref(),
            Some("18 pub/sub call(s) left out of the index: their events stay inside the service.")
        );
        assert_eq!(run.summary_line(), None);
        assert!(run.service_losses(&Some("api".to_string())).is_empty());
    }

    /// carrick#2496: the packages a scan ended without are named in one
    /// sentence each (still pending, could not be built), are not a loss, and
    /// a service that asked and ended without none counts zero while one that
    /// never asked counts nothing.
    #[test]
    fn packages_the_scan_ended_without_are_named_and_counted_not_lost() {
        let mut run = run();
        assert!(run.library_pending_lines().is_empty());
        let api = Some("api".to_string());
        let web = Some("web".to_string());
        let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect();
        run.record_library_packages(
            &api,
            LibraryUnready {
                pending: names(&["b", "a"]),
                failed: names(&["z"]),
            },
        );
        run.record_library_packages(&web, Default::default());
        assert_eq!(run.library_packages_for(&api), Some((2, 1)));
        assert_eq!(run.library_packages_for(&web), Some((0, 0)));
        assert_eq!(run.library_packages_for(&Some("other".to_string())), None);
        assert_eq!(
            run.library_pending_lines(),
            [
                "2 libraries were not ready in time, so calls through them may be missing from this index: a, b. The next scan adds them.",
                "Carrick could not read 1 library yet, so calls through it are missing from this index: z.",
            ]
        );
        assert_eq!(run.summary_line(), None);
        assert!(run.service_losses(&api).is_empty());
        run.record_library_packages(
            &api,
            LibraryUnready {
                pending: (0..7).map(|n| format!("p{n}")).collect(),
                failed: (0..6).map(|n| format!("f{n}")).collect(),
            },
        );
        assert_eq!(
            run.library_pending_lines(),
            [
                "7 libraries were not ready in time, so calls through them may be missing from this index: p0, p1, p2, p3, p4 and 2 more. The next scan adds them.",
                "Carrick could not read 6 libraries yet, so calls through them are missing from this index: f0, f1, f2, f3, f4 and 1 more.",
            ]
        );
        run.record_library_packages(
            &api,
            LibraryUnready {
                pending: names(&["only"]),
                failed: Default::default(),
            },
        );
        assert_eq!(
            run.library_pending_lines(),
            [
                "1 library was not ready in time, so calls through it may be missing from this index: only. The next scan adds it."
            ]
        );
        run.forget_service_losses(&api);
        assert!(run.library_pending_lines().is_empty());
    }

    /// The regression: one lost file must reach the summary AND its service's
    /// owed work, which is what holds a service with an index back (#461).
    #[test]
    fn a_lost_file_is_named_and_owed_by_its_service() {
        let mut run = run();
        run.record_files_attempted(3);
        run.record_unanalysed_file(
            &None,
            "src/routes/orders.ts",
            &LossReason::owed("gateway_error"),
        );

        let summary = run
            .summary_line()
            .expect("a lost file must produce a summary");
        assert!(
            summary.starts_with("1 of 3 files were not analysed: 1 gateway_error"),
            "summary: {summary}"
        );
        assert!(
            summary.contains("src/routes/orders.ts"),
            "summary: {summary}"
        );
        assert_eq!(run.service_losses(&None).files, 1);
    }

    /// Losses belong to the service being analysed when they happen, so one
    /// service's upload carries its own unanalysed files and a retry of it
    /// forgets only its own.
    #[test]
    fn losses_are_scoped_to_the_service_that_had_them() {
        let api = Some("api".to_string());
        let billing = Some("billing".to_string());
        let mut run = run();
        run.record_files_attempted(4);
        run.record_unanalysed_file(&api, "api/src/a.ts", &LossReason::owed("model_error"));
        run.record_files_attempted(2);
        run.record_unanalysed_file(
            &billing,
            "billing/src/b.ts",
            &LossReason::owed("gateway_error"),
        );
        run.record_intents_failed(&billing, 3, 0);

        assert_eq!(run.service_losses(&api).files, 1);
        assert_eq!(run.service_losses(&api).intents, 0);
        assert_eq!(run.service_losses(&billing).intents, 3);
        let api_files = run.unanalysed_files_for(&api);
        assert_eq!(api_files.len(), 1);
        assert_eq!(api_files[0].path, "api/src/a.ts");

        run.forget_service_losses(&billing);
        assert!(run.service_losses(&billing).is_empty());
        assert_eq!(run.service_losses(&api).files, 1);
        assert_eq!(run.lost_file_count(), 1);
        // The forgotten file leaves the attempted count, because the retry
        // dispatches and counts it again.
        assert_eq!(run.attempted, 5);
    }

    /// carrick#1066. Two services analysed side by side, their awaits
    /// interleaving, each file their losses under their own name, including
    /// the ones recorded on a task the service started through the helper.
    /// Names no other test uses: the registry is the process's.
    #[tokio::test]
    async fn losses_recorded_side_by_side_are_each_services_own() {
        const LEFT: &str = "side-by-side-left-1066";
        const RIGHT: &str = "side-by-side-right-1066";
        forget_service_losses(Some(LEFT));
        forget_service_losses(Some(RIGHT));
        let analyse = |service: &'static str, files: usize, intents: usize| {
            crate::current_service::scope(Some(service.to_string()), async move {
                for n in 0..files {
                    tokio::task::yield_now().await;
                    record_unanalysed_file(&format!("{service}/{n}.ts"), &LossReason::owed("x"));
                }
                crate::current_service::spawn(async move { record_intents_failed(intents, 0) })
                    .await
                    .unwrap();
            })
        };
        tokio::join!(analyse(LEFT, 3, 5), analyse(RIGHT, 1, 2));

        let (left, right) = (service_losses(Some(LEFT)), service_losses(Some(RIGHT)));
        assert_eq!((left.files, left.intents), (3, 5));
        assert_eq!((right.files, right.intents), (1, 2));
        assert_eq!(unanalysed_files_for(Some(RIGHT)).len(), 1);
        forget_service_losses(Some(LEFT));
        forget_service_losses(Some(RIGHT));
    }

    /// carrick#1896: what a service owes is split by whether waiting can mend
    /// it. A file or an intent the model refused for capacity is counted as
    /// one; a file the gateway cut, a lost connection and an intent the model
    /// answered with nothing are not, whatever their number.
    #[test]
    fn only_what_the_model_refused_for_capacity_is_counted_as_such() {
        let api = Some("api".to_string());
        let billing = Some("billing".to_string());
        let mut run = run();
        run.record_unanalysed_file(&api, "api/src/a.ts", &LossReason::owed("gateway_error"));
        run.record_unanalysed_file(&api, "api/src/b.ts", &LossReason::owed("network_error"));
        run.record_unanalysed_file(
            &api,
            "api/src/c.ts",
            &LossReason::owed("analysis_in_flight"),
        );
        run.record_intents_failed(&api, 4, 0);
        run.record_unanalysed_file(
            &billing,
            "billing/src/a.ts",
            &LossReason::owed(crate::agent_service::CAPACITY_REFUSAL_CODE),
        );
        run.record_unanalysed_file(
            &billing,
            "billing/src/b.ts",
            &LossReason::owed("gateway_error"),
        );
        run.record_intents_failed(&billing, 5, 2);
        run.record_intents_failed(&billing, 1, 1);

        assert_eq!(
            run.service_losses(&api),
            ServiceLosses {
                files: 3,
                intents: 4,
                capacity_files: 0,
                capacity_intents: 0,
                final_files: 0,
            }
        );
        assert_eq!(
            run.service_losses(&billing),
            ServiceLosses {
                files: 2,
                intents: 6,
                capacity_files: 1,
                capacity_intents: 3,
                final_files: 0,
            }
        );

        run.forget_service_losses(&billing);
        assert_eq!(run.service_losses(&billing), ServiceLosses::default());
    }

    /// carrick#1909: a route that has stopped answering refuses one file's
    /// request and the files behind it are not sent. Both are lost files of
    /// the run, named and owed by their service, and both are counted as
    /// owed for capacity and never as a final answer: the provider was busy,
    /// which says nothing about either file. The summary tells them apart by
    /// code, the cloud's for the file that was asked and the scanner's own
    /// for the ones that were not. The retry of owed work forgets them all
    /// and asks again.
    #[test]
    fn files_behind_a_refusal_on_a_silent_route_are_owed_for_capacity_and_told_apart() {
        let refused = AgentCallError::from_cloud(
            crate::agent_service::CAPACITY_REFUSAL_CODE,
            "model busy",
            true,
        );
        let unasked = crate::agent_service::not_asked("/analyze-file");
        for error in [&refused, &unasked] {
            assert!(counts_as_lost_file(error));
            assert!(!is_budget_refusal(error));
            assert!(!analysis_failure_reason(error).is_final, "{error}");
        }
        assert_eq!(
            analysis_failure_reason(&unasked),
            LossReason::owed(crate::agent_service::NOT_ASKED_CODE)
        );

        let api = Some("api".to_string());
        let mut run = run();
        run.record_files_attempted(3);
        run.record_unanalysed_file(&api, "api/src/a.ts", &analysis_failure_reason(&refused));
        run.record_unanalysed_file(&api, "api/src/b.ts", &analysis_failure_reason(&unasked));
        run.record_unanalysed_file(&api, "api/src/c.ts", &analysis_failure_reason(&unasked));

        assert_eq!(
            run.service_losses(&api),
            ServiceLosses {
                files: 3,
                intents: 0,
                capacity_files: 3,
                capacity_intents: 0,
                final_files: 0,
            }
        );
        assert_eq!(
            run.summary_line().as_deref(),
            Some(
                "3 of 3 files were not analysed: 2 model_busy_not_asked, 1 model_error. Their \
                 endpoints and calls are missing from this run's results (api/src/a.ts, \
                 api/src/b.ts, api/src/c.ts)"
            )
        );
        let named: Vec<(String, String)> = run
            .unanalysed_files_for(&api)
            .into_iter()
            .map(|file| (file.path, file.reason))
            .collect();
        assert_eq!(
            named,
            [
                ("api/src/a.ts".to_string(), "model_error".to_string()),
                (
                    "api/src/b.ts".to_string(),
                    "model_busy_not_asked".to_string()
                ),
                (
                    "api/src/c.ts".to_string(),
                    "model_busy_not_asked".to_string()
                ),
            ]
        );

        run.forget_service_losses(&api);
        assert!(run.service_losses(&api).is_empty());
        assert_eq!(run.attempted, 0);
    }

    /// Reasons are grouped and ordered by how many files each cost, so the
    /// dominant cause is the first thing read.
    #[test]
    fn reasons_are_grouped_most_frequent_first() {
        let mut run = run();
        run.record_files_attempted(2987);
        for i in 0..8 {
            run.record_unanalysed_file(
                &None,
                &format!("a/{i}.ts"),
                &LossReason::owed("gateway_error"),
            );
        }
        for i in 0..3 {
            run.record_unanalysed_file(
                &None,
                &format!("b/{i}.ts"),
                &LossReason::owed("model_error"),
            );
        }
        run.record_unanalysed_file(&None, "c/0.ts", &LossReason::owed("oidc_rejected"));

        let summary = run.summary_line().unwrap();
        assert!(
            summary.starts_with(
                "12 of 2987 files were not analysed: 8 gateway_error, 3 model_error, \
                 1 oidc_rejected"
            ),
            "summary: {summary}"
        );
        // Twelve paths, ten named.
        assert!(summary.contains("and 2 more"), "summary: {summary}");
    }

    /// A run that lost its type layer says so on the same line it reports the
    /// rest of its health, naming the scope and the reason.
    #[test]
    fn a_typeless_run_names_the_scope_and_the_reason() {
        let mut run = run();
        assert_eq!(run.types_summary_line(), None);

        run.record_types_unavailable(WHOLE_SCAN, "Sidecar operation timed out");

        let summary = run
            .types_summary_line()
            .expect("a lost type layer must produce a summary");
        assert!(
            summary.contains("this scan (Sidecar operation timed out)"),
            "summary: {summary}"
        );
        // What it cost, not just that it happened.
        assert!(
            summary.contains("no request or response types"),
            "summary: {summary}"
        );
        assert_eq!(
            run.types_unavailable_reason(WHOLE_SCAN).as_deref(),
            Some("Sidecar operation timed out")
        );
        assert_eq!(run.types_unavailable_reason("webapp"), None);
    }

    /// One failure seen by two stages is one loss. The per-service re-init and
    /// the resolve step both notice the same dead sidecar.
    #[test]
    fn a_scope_is_recorded_once_with_its_first_reason() {
        let mut run = run();
        run.record_types_unavailable("webapp", "Sidecar operation timed out");
        run.record_types_unavailable("webapp", "Sidecar not ready: already failed");

        let summary = run.types_summary_line().unwrap();
        assert!(
            summary.contains("webapp (Sidecar operation timed out)"),
            "summary: {summary}"
        );
        assert!(!summary.contains("already failed"), "summary: {summary}");
    }

    /// The fail/warn split: an environmental failure stops the run, a
    /// repeatable one never does, and the opt-out overrides the first.
    #[test]
    fn only_an_environmental_type_loss_fails_the_run() {
        use crate::services::type_sidecar::SidecarError;

        assert!(SidecarError::Timeout.is_environmental());
        assert!(SidecarError::ProcessDied.is_environmental());
        assert!(SidecarError::IoError("broken pipe".into()).is_environmental());
        // A sidecar that answered with a complaint, or is not installed,
        // would fail identically on every run.
        assert!(!SidecarError::InitFailed("bad tsconfig".into()).is_environmental());
        assert!(!SidecarError::SpawnFailed("no node".into()).is_environmental());

        assert!(should_fail_on_missing_types(true, false));
        assert!(!should_fail_on_missing_types(true, true));
        assert!(!should_fail_on_missing_types(false, false));
    }

    /// The reason comes from the call's own error code when there is one. A
    /// rejected token is the scanner's own reading of a failure, permanent
    /// for the call and nothing the cloud said about the request, so the file
    /// is owed as it always was.
    #[test]
    fn failure_reason_reads_the_cloud_error_code() {
        let call_error: Box<dyn std::error::Error> = Box::new(AgentCallError::permanent(
            "oidc_rejected",
            "token expired".to_string(),
        ));
        assert_eq!(
            analysis_failure_reason(call_error.as_ref()),
            LossReason::owed("oidc_rejected")
        );

        let other: Box<dyn std::error::Error> = "not JSON".into();
        assert_eq!(
            analysis_failure_reason(other.as_ref()),
            LossReason::owed("unparseable_response")
        );
    }

    /// carrick#1897: a file is final only on the cloud's own word. An error
    /// envelope with `retriable: false` is one whatever its code, a code this
    /// build has never heard of included. A retriable envelope is not, a
    /// failure the scanner classified itself is not, and a limit of the
    /// cloud's never reaches the lost files at all.
    #[test]
    fn a_file_is_final_only_when_the_cloud_said_its_request_cannot_succeed() {
        fn final_answer(error: &(dyn std::error::Error + 'static)) -> bool {
            analysis_failure_reason(error).is_final
        }
        assert!(final_answer(&AgentCallError::from_cloud(
            "output_truncated",
            "every try was cut",
            false
        )));
        assert!(final_answer(&AgentCallError::from_cloud(
            "a_later_verdict",
            "no",
            false
        )));
        assert!(!final_answer(&AgentCallError::from_cloud(
            "model_error",
            "overloaded",
            true
        )));
        assert!(!final_answer(&AgentCallError::permanent(
            "bad_response",
            "not an envelope".to_string()
        )));
        let refused = AgentCallError::from_cloud(
            crate::agent_service::LLM_DISABLED_CODE,
            "past the allowance",
            false,
        );
        assert!(!counts_as_lost_file(&refused));
        assert!(!refused.is_final_answer());
        assert!(!final_answer(&std::io::Error::other("connection reset")));
    }

    /// What a service owes says how many of its lost files are final, the
    /// summary names them by the code they ended on, and the upload names a
    /// final one apart from the owed ones, by its code (carrick#2488).
    #[test]
    fn final_files_are_counted_apart_and_named_by_their_code() {
        let truncated = AgentCallError::from_cloud("output_truncated", "every try was cut", false);
        let mut run = run();
        run.record_files_attempted(3);
        run.record_unanalysed_file(&None, "src/routes.ts", &analysis_failure_reason(&truncated));
        run.record_unanalysed_file(&None, "src/orders.ts", &LossReason::owed("gateway_error"));

        assert_eq!(
            run.service_losses(&None),
            ServiceLosses {
                files: 2,
                final_files: 1,
                ..ServiceLosses::default()
            }
        );
        let summary = run.summary_line().expect("two files were lost");
        assert!(
            summary
                .starts_with("2 of 3 files were not analysed: 1 gateway_error, 1 output_truncated"),
            "{summary}"
        );
        let owed = run.unanalysed_files_for(&None);
        assert_eq!(
            owed,
            vec![crate::cloud_storage::UnanalysedFile {
                path: "src/orders.ts".to_string(),
                reason: "gateway_error".to_string(),
            }],
            "only a file another ask may mend is owed"
        );
        let final_losses = run.final_losses_for(&None);
        assert_eq!(
            final_losses,
            vec![crate::cloud_storage::UnanalysedFile {
                path: "src/routes.ts".to_string(),
                reason: "output_truncated".to_string(),
            }],
            "a final loss is named apart, under its code"
        );
    }

    /// The sentence that stands where "run again" would: how many files, that
    /// a re-run changes nothing, and what would. Short enough to read.
    #[test]
    fn the_final_answer_sentence_says_what_a_rerun_does() {
        assert_eq!(final_answer_sentence(0), None);
        let sentence = final_answer_sentence(2).unwrap();
        assert_eq!(
            sentence,
            "The answer for 2 file(s) is final: a re-run gives the same result until the file \
             changes or Carrick is updated."
        );
        assert!(
            sentence.split_whitespace().count() <= 22,
            "a line a person reads, not a paragraph: {sentence}"
        );
    }

    /// carrick#2021: a time limit that runs out is counted against the
    /// service being analysed, by kind, and a service analysed again counts
    /// afresh rather than twice. A cost of nothing records nothing, so a
    /// service no limit cut short has no count at all.
    #[test]
    fn a_time_limit_is_counted_against_the_service_being_analysed() {
        use crate::time_limits::{TimeLimit, TimeLimitsRunOut};
        let api = Some("api".to_string());
        let web = Some("web".to_string());
        let mut run = run();

        run.record_time_limit(&api, TimeLimit::LibraryChecks, 3);
        run.record_time_limit(&api, TimeLimit::SidecarDeadline, 50);
        run.record_time_limit(&api, TimeLimit::LibraryChecks, 2);
        run.record_time_limit(&web, TimeLimit::LibraryChecks, 0);

        assert_eq!(
            run.time_limits_for(&api),
            TimeLimitsRunOut {
                library_checks: 5,
                sidecar_deadline: 50,
                ..TimeLimitsRunOut::default()
            }
        );
        assert!(run.time_limits_for(&web).is_empty());

        run.forget_service_losses(&api);
        run.record_time_limit(&api, TimeLimit::LibraryChecks, 1);
        assert_eq!(
            run.time_limits_for(&api),
            TimeLimitsRunOut {
                library_checks: 1,
                ..TimeLimitsRunOut::default()
            }
        );
    }

    /// carrick#2208: a recorded memory death stops the run only when it was
    /// asked to stop on one, and then at the first; unset, nothing changes.
    #[test]
    fn the_first_memory_death_stops_only_a_run_told_to_stop() {
        use crate::scan_stage::Stage;
        use crate::services::type_sidecar::MemoryDeath;
        let capture = TypeMemoryDeath {
            service: Some("beta".to_string()),
            stage: Stage::TypeCapture,
            kind: MemoryDeath::HeapLimit,
            heap_cap_mb: Some(6144),
            in_analysis: true,
            beside: false,
        };
        let check = TypeMemoryDeath {
            service: None,
            stage: Stage::CrossRepoCheck,
            kind: MemoryDeath::OomKilled,
            heap_cap_mb: None,
            in_analysis: false,
            beside: false,
        };
        let run_cap = Some(6144);
        assert_eq!(stop_for_type_memory(true, &[], run_cap), Ok(()));
        assert_eq!(
            stop_for_type_memory(false, std::slice::from_ref(&capture), run_cap),
            Ok(())
        );
        let deaths = [capture.clone(), check.clone()];
        assert_eq!(
            stop_for_type_memory(true, &deaths, run_cap),
            Err(TypesOutOfMemory(capture.clone()))
        );
        assert_eq!(
            TypesOutOfMemory(capture).to_string(),
            "types_out_of_memory: the type checker reached its heap limit (heap_limit) during \
             type_capture for service beta, under a heap cap of 6144 MB; the run stopped so it \
             can run again on a larger machine"
        );
        assert_eq!(
            TypesOutOfMemory(check).to_string(),
            "types_out_of_memory: the type checker was killed by the kernel for memory \
             (oom_killed) during cross_repo_check, under a heap cap of V8's default; the run \
             stopped so it can run again on a larger machine"
        );
    }

    /// carrick#3084: in a service's analysis, only a death alone at the run's
    /// cap stops the run. One beside another service, or under a lower cap,
    /// is that service's to analyse again alone. A death outside any
    /// service's analysis stops it whatever cap it records.
    #[test]
    fn only_a_death_alone_at_the_run_cap_stops_the_run() {
        use crate::scan_stage::Stage;
        use crate::services::type_sidecar::MemoryDeath;
        let run_cap = Some(12_288);
        let death = |heap_cap_mb, in_analysis, beside| TypeMemoryDeath {
            service: Some("beta".to_string()),
            stage: Stage::TypeCapture,
            kind: MemoryDeath::HeapLimit,
            heap_cap_mb,
            in_analysis,
            beside,
        };
        let beside = death(run_cap, true, true);
        let under_its_own_cap = death(Some(2_048), true, false);
        let alone = death(run_cap, true, false);
        let after_the_loop = death(Some(2_048), false, false);
        assert_eq!(
            stop_for_type_memory(true, &[beside.clone(), under_its_own_cap.clone()], run_cap),
            Ok(())
        );
        assert_eq!(
            stop_for_type_memory(true, &[beside.clone(), alone.clone()], run_cap),
            Err(TypesOutOfMemory(alone.clone()))
        );
        assert_eq!(
            stop_for_type_memory(true, &[beside, after_the_loop.clone()], run_cap),
            Err(TypesOutOfMemory(after_the_loop))
        );
        // The run's cap turned off: a death at V8's default, alone, stops it.
        assert_eq!(
            stop_for_type_memory(true, &[death(None, true, false)], None),
            Err(TypesOutOfMemory(death(None, true, false)))
        );
    }
}
