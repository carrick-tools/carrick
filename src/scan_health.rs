//! Run-level record of analysis the scan was supposed to do and did not.
//!
//! A file whose analyzer call fails after its retries are spent is simply
//! absent from `file_results`: its endpoints and its outbound calls are not in
//! the index, and the matched rows on the other side of them disappear too. The
//! scan itself does not notice — the fold that collects per-file results counts
//! the failure and carries on, which is how a run could report success while
//! silently removing most of a service's endpoints (#461).
//!
//! This module is where those losses are counted, so the end of the run can
//! state them and refuse to call itself a success. It is a process-global for
//! the same reason [`crate::agent_service::rate_limit_tripped`] is: a scan is
//! one process, several independently-constructed services analyse inside it,
//! and the question "did this run lose anything" is about the run, not about
//! any one of them.
//!
//! What belongs here is loss the scan cannot account for: a call the cloud
//! never answered. Deterministic exclusions do not — a file that fails to parse
//! is a known, repeatable limitation, and putting it here would make a repo
//! with one unparseable file permanently red.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Set this to let a run finish green despite losing files. The loss is still
/// reported; only the verdict changes.
pub const ALLOW_PARTIAL_ENV: &str = "CARRICK_ALLOW_PARTIAL_ANALYSIS";

/// How many lost files are named individually before the list is truncated.
const MAX_NAMED_FILES: usize = 10;

#[derive(Default)]
struct Registry {
    /// Files dispatched to the analyzer across every service in this run.
    attempted: usize,
    /// One entry per file the analyzer never answered for: (path, reason code).
    lost: Vec<(String, String)>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Adds a service's dispatched file count to the run total. Called once per
/// service, since a repo can hold several.
pub fn record_files_attempted(count: usize) {
    registry().lock().expect("scan health lock").attempted += count;
}

/// Records that `path` has no analysis in this run's index, and why.
///
/// `reason` is a stable code (the cloud's error code, or a scanner-side
/// pseudo-code), not a sentence: the summary groups by it.
pub fn record_unanalysed_file(path: &str, reason: &str) {
    registry()
        .lock()
        .expect("scan health lock")
        .lost
        .push((path.to_string(), reason.to_string()));
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
    registry().lock().expect("scan health lock").lost.len()
}

/// How many files this run sent to the analyzer.
pub fn attempted_count() -> usize {
    registry().lock().expect("scan health lock").attempted
}

/// One line naming what the run lost and why, or `None` when it lost nothing.
///
/// Grouped by reason and ordered most-frequent first, because the useful fact
/// is the cause: twelve files lost to one expired token is a different incident
/// from twelve lost to twelve different failures.
pub fn summary_line() -> Option<String> {
    let guard = registry().lock().expect("scan health lock");
    if guard.lost.is_empty() {
        return None;
    }

    let mut by_reason: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, reason) in &guard.lost {
        *by_reason.entry(reason.as_str()).or_default() += 1;
    }
    let mut reasons: Vec<(&str, usize)> = by_reason.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let reasons = reasons
        .iter()
        .map(|(reason, count)| format!("{} {}", count, reason))
        .collect::<Vec<_>>()
        .join(", ");

    let mut paths: Vec<&str> = guard.lost.iter().map(|(path, _)| path.as_str()).collect();
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
        guard.lost.len(),
        guard.attempted,
        reasons,
        named,
        and_more
    ))
}

/// Whether [`ALLOW_PARTIAL_ENV`] is set for this run.
pub fn allow_partial_from_env() -> bool {
    std::env::var(ALLOW_PARTIAL_ENV).is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Whether the run must fail. Pure, so the policy is testable without touching
/// the process environment: any lost file fails the run unless the operator
/// asked for a partial result.
pub fn should_fail_run(lost: usize, allow_partial: bool) -> bool {
    lost > 0 && !allow_partial
}

#[cfg(test)]
pub(crate) fn reset() {
    let mut guard = registry().lock().expect("scan health lock");
    guard.attempted = 0;
    guard.lost.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_service::AgentCallError;

    /// A run that lost nothing says nothing and passes.
    #[test]
    fn a_clean_run_has_no_summary_and_does_not_fail() {
        reset();
        record_files_attempted(120);
        assert_eq!(summary_line(), None);
        assert!(!should_fail_run(lost_file_count(), false));
    }

    /// The regression: one lost file must reach the summary AND the exit code.
    #[test]
    fn a_lost_file_is_named_and_fails_the_run() {
        reset();
        record_files_attempted(3);
        record_unanalysed_file("src/routes/orders.ts", "gateway_error");

        let summary = summary_line().expect("a lost file must produce a summary");
        assert!(
            summary.starts_with("1 of 3 files were not analysed: 1 gateway_error"),
            "summary: {summary}"
        );
        assert!(
            summary.contains("src/routes/orders.ts"),
            "summary: {summary}"
        );
        assert!(should_fail_run(lost_file_count(), false));
        // Only an explicit opt-in keeps such a run green.
        assert!(!should_fail_run(lost_file_count(), true));
        reset();
    }

    /// Reasons are grouped and ordered by how many files each cost, so the
    /// dominant cause is the first thing read.
    #[test]
    fn reasons_are_grouped_most_frequent_first() {
        reset();
        record_files_attempted(2987);
        for i in 0..8 {
            record_unanalysed_file(&format!("a/{i}.ts"), "gateway_error");
        }
        for i in 0..3 {
            record_unanalysed_file(&format!("b/{i}.ts"), "model_error");
        }
        record_unanalysed_file("c/0.ts", "oidc_rejected");

        let summary = summary_line().unwrap();
        assert!(
            summary.starts_with(
                "12 of 2987 files were not analysed: 8 gateway_error, 3 model_error, \
                 1 oidc_rejected"
            ),
            "summary: {summary}"
        );
        // Twelve paths, ten named.
        assert!(summary.contains("and 2 more"), "summary: {summary}");
        reset();
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
