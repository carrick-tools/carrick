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
//! state them and refuse to call itself a success. It is a process-global for
//! the same reason [`crate::agent_service::rate_limit_tripped`] is: a scan is
//! one process, several independently-constructed services analyse inside it,
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

/// How many lost files are named individually before the list is truncated.
const MAX_NAMED_FILES: usize = 10;

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
    /// One entry per file the analyzer never answered for: (path, reason code).
    lost: Vec<(String, String)>,
    /// One entry per file the model was deliberately not asked about, because
    /// a budget refused the call. Not a loss the scan can fix by re-running,
    /// and not a reason to fail: the file keeps its deterministic rows and its
    /// candidates are simply not refreshed this run (carrick#555).
    not_refreshed: Vec<String>,
    /// One entry per scope that got no type layer: (scope, reason). The scope
    /// is a service name, or [`WHOLE_SCAN`] when the sidecar never became
    /// ready at all and every service in the run is typeless.
    types_unavailable: Vec<(String, String)>,
}

impl Registry {
    /// Adds a service's dispatched file count to the total.
    fn record_files_attempted(&mut self, count: usize) {
        self.attempted += count;
    }

    /// Records that `path` has no analysis, and why.
    fn record_unanalysed_file(&mut self, path: &str, reason: &str) {
        self.lost.push((path.to_string(), reason.to_string()));
    }

    /// Records that the model was not asked about `path`, on purpose.
    fn record_candidates_not_refreshed(&mut self, path: &str) {
        self.not_refreshed.push(path.to_string());
    }

    /// One line naming how many files the budget refused, or `None`.
    ///
    /// Deliberately says nothing about which budget or until when: the cloud
    /// owns that sentence (`allowance_sentence`, printed at the top of the
    /// run) and a second, guessed version of it here would contradict it.
    fn not_refreshed_line(&self) -> Option<String> {
        if self.not_refreshed.is_empty() {
            return None;
        }
        Some(format!(
            "{} of {} files were not sent to the model: this workspace is past an inference \
             allowance. They keep the rows the deterministic layer stated and their candidates \
             were not refreshed this run",
            self.not_refreshed.len(),
            self.attempted
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
        for (_, reason) in &self.lost {
            *by_reason.entry(reason.as_str()).or_default() += 1;
        }
        let mut reasons: Vec<(&str, usize)> = by_reason.into_iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let reasons = reasons
            .iter()
            .map(|(reason, count)| format!("{} {}", count, reason))
            .collect::<Vec<_>>()
            .join(", ");

        let mut paths: Vec<&str> = self.lost.iter().map(|(path, _)| path.as_str()).collect();
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

/// Records that `path` has no analysis in this run's index, and why.
///
/// `reason` is a stable code (the cloud's error code, or a scanner-side
/// pseudo-code), not a sentence: the summary groups by it.
pub fn record_unanalysed_file(path: &str, reason: &str) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_unanalysed_file(path, reason);
}

/// Records that the model was deliberately not asked about `path`.
///
/// Not a lost file: nothing failed, a budget said no. The run does not fail,
/// the file is not named in `unanalysed_files` on the upload, and the index
/// keeps every row the deterministic layer stated about it.
pub fn record_candidates_not_refreshed(path: &str) {
    registry()
        .lock()
        .expect("scan health lock")
        .record_candidates_not_refreshed(path);
}

/// Whether a failed analyzer call is a file this run LOST, as opposed to one
/// it was told not to ask about.
///
/// Two codes are not losses, for different reasons:
///
/// - [`crate::agent_service::QUOTA_ABORT_CODE`]: the process-global breaker is
///   open, so this call was never attempted. The breaker fails the run on its
///   own terms.
/// - [`crate::agent_service::LLM_DISABLED_CODE`]: a budget refused the call,
///   whatever `details.reason` says. The ruling is that the scan completes
///   facts-only and never fails, so counting these would abort the run before
///   the upload and leave the organisation with no index (carrick#555).
///
/// Everything else — the model was asked and did not answer — is a loss.
pub fn counts_as_lost_file(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<crate::agent_service::AgentCallError>()
        .is_none_or(|e| !e.is_quota_abort() && !e.is_budget_refusal())
}

/// Whether a failed analyzer call means a budget refused it.
pub fn is_budget_refusal(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<crate::agent_service::AgentCallError>()
        .is_some_and(|e| e.is_budget_refusal())
}

/// Reason code for a failed file analysis, for [`record_unanalysed_file`].
///
/// Reads the cloud's own error code where the failure came from a lambda call,
/// so the summary says `gateway_error` or `oidc_rejected` rather than a
/// paragraph. Anything else is a malformed answer rather than an absent one.
pub fn analysis_failure_reason(error: &(dyn std::error::Error + 'static)) -> String {
    error
        .downcast_ref::<crate::agent_service::AgentCallError>()
        .map(|e| e.code.clone())
        .unwrap_or_else(|| "unparseable_response".to_string())
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

/// The files this run lost, in the shape the upload reports them.
///
/// Only the second category: a file the model was asked about and did not
/// answer for. A file a budget refused is never here, so the cloud's
/// first-scan partial rule is never asked to accept a partial index that is
/// only partial because the organisation is out of allowance.
pub fn unanalysed_files() -> Vec<crate::cloud_storage::UnanalysedFile> {
    registry()
        .lock()
        .expect("scan health lock")
        .lost
        .iter()
        .map(|(path, reason)| crate::cloud_storage::UnanalysedFile {
            path: path.clone(),
            reason: reason.clone(),
        })
        .collect()
}

/// Whether [`ALLOW_PARTIAL_ENV`] is set for this run.
pub fn allow_partial_from_env() -> bool {
    env_flag(ALLOW_PARTIAL_ENV)
}

/// Whether [`ALLOW_MISSING_TYPES_ENV`] is set for this run.
pub fn allow_missing_types_from_env() -> bool {
    env_flag(ALLOW_MISSING_TYPES_ENV)
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

/// Whether the run must fail. Pure, so the policy is testable without touching
/// the process environment: any lost file fails the run unless the operator
/// asked for a partial result.
pub fn should_fail_run(lost: usize, allow_partial: bool) -> bool {
    lost > 0 && !allow_partial
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
        let refused = AgentCallError {
            code: crate::agent_service::LLM_DISABLED_CODE.to_string(),
            message: "past the monthly allowance".to_string(),
            retriable: false,
        };
        assert!(!counts_as_lost_file(&refused));
        assert!(is_budget_refusal(&refused));

        let mut registry = run();
        registry.record_files_attempted(3);
        for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
            assert!(!counts_as_lost_file(&refused));
            registry.record_candidates_not_refreshed(path);
        }

        assert_eq!(registry.lost_file_count(), 0);
        assert!(registry.summary_line().is_none(), "nothing was lost");
        assert!(!should_fail_run(registry.lost_file_count(), false));

        let line = registry.not_refreshed_line().expect("the run says so");
        assert!(
            line.contains("3 of 3 files were not sent to the model"),
            "{line}"
        );
        assert!(line.contains("not refreshed"), "{line}");
    }

    /// The other half of the split, which must keep working exactly as it did:
    /// the model WAS asked and did not answer, so the file is lost, the run
    /// fails, and the upload is skipped so a thinner index cannot overwrite a
    /// good one (#461).
    #[test]
    fn a_call_that_was_made_and_failed_is_still_a_lost_file() {
        let failed = AgentCallError {
            code: "model_error".to_string(),
            message: "the model returned nothing".to_string(),
            retriable: true,
        };
        assert!(counts_as_lost_file(&failed));
        assert!(!is_budget_refusal(&failed));

        let mut registry = run();
        registry.record_files_attempted(2);
        registry.record_unanalysed_file("src/a.ts", &analysis_failure_reason(&failed));
        assert_eq!(registry.lost_file_count(), 1);
        assert!(should_fail_run(registry.lost_file_count(), false));
        assert!(registry.not_refreshed_line().is_none());
        assert!(registry.summary_line().unwrap().contains("model_error"));
    }

    /// A call the quota breaker aborted was never attempted, and the breaker
    /// fails the run on its own terms. It is neither category.
    #[test]
    fn a_quota_abort_is_neither_a_loss_nor_a_budget_refusal() {
        let aborted = AgentCallError {
            code: crate::agent_service::QUOTA_ABORT_CODE.to_string(),
            message: "breaker open".to_string(),
            retriable: false,
        };
        assert!(!counts_as_lost_file(&aborted));
        assert!(!is_budget_refusal(&aborted));
    }

    /// A failure that never produced an envelope is a loss: the scanner cannot
    /// know it was refused on purpose, so it must not assume it was.
    #[test]
    fn an_untyped_failure_counts_as_a_loss() {
        let untyped = std::io::Error::other("connection reset");
        assert!(counts_as_lost_file(&untyped));
        assert!(!is_budget_refusal(&untyped));
        assert_eq!(analysis_failure_reason(&untyped), "unparseable_response");
    }

    /// A run that lost nothing says nothing and passes.
    #[test]
    fn a_clean_run_has_no_summary_and_does_not_fail() {
        let mut run = run();
        run.record_files_attempted(120);
        assert_eq!(run.summary_line(), None);
        assert!(!should_fail_run(run.lost_file_count(), false));
    }

    /// The regression: one lost file must reach the summary AND the exit code.
    #[test]
    fn a_lost_file_is_named_and_fails_the_run() {
        let mut run = run();
        run.record_files_attempted(3);
        run.record_unanalysed_file("src/routes/orders.ts", "gateway_error");

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
        assert!(should_fail_run(run.lost_file_count(), false));
        // Only an explicit opt-in keeps such a run green.
        assert!(!should_fail_run(run.lost_file_count(), true));
    }

    /// Reasons are grouped and ordered by how many files each cost, so the
    /// dominant cause is the first thing read.
    #[test]
    fn reasons_are_grouped_most_frequent_first() {
        let mut run = run();
        run.record_files_attempted(2987);
        for i in 0..8 {
            run.record_unanalysed_file(&format!("a/{i}.ts"), "gateway_error");
        }
        for i in 0..3 {
            run.record_unanalysed_file(&format!("b/{i}.ts"), "model_error");
        }
        run.record_unanalysed_file("c/0.ts", "oidc_rejected");

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

    /// The reason comes from the cloud's own error code when there is one.
    #[test]
    fn failure_reason_reads_the_cloud_error_code() {
        let call_error: Box<dyn std::error::Error> = Box::new(AgentCallError {
            code: "oidc_rejected".to_string(),
            message: "token expired".to_string(),
            retriable: false,
        });
        assert_eq!(
            analysis_failure_reason(call_error.as_ref()),
            "oidc_rejected"
        );

        let other: Box<dyn std::error::Error> = "not JSON".into();
        assert_eq!(
            analysis_failure_reason(other.as_ref()),
            "unparseable_response"
        );
    }
}
