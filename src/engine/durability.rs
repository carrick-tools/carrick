//! What a scan does when part of it cannot be finished (2026-09-15).
//!
//! A laptop first index of a seven-service monorepo ran for seventeen minutes,
//! finished four services, and then one framework-detection call for the fifth
//! came back `model_error` seven times. The call's `?` ended the run, and
//! nothing uploaded, because the upload runs once, after every service.
//!
//! The rules this module holds, in the order a run meets them:
//!
//! 1. A service whose detection or guidance cannot be had is DEFERRED
//!    ([`ModelSetup::deferred`]): analysed facts-only, nothing cached that the
//!    model did not answer, the run carries on. A detection that answered is
//!    kept when only its guidance failed ([`ModelSetup::guidance_deferred`]).
//! 2. Once every service has been through, the work still owed — a deferred
//!    service, files the analyzer never answered for, intents that failed, calls
//!    the quota breaker stopped — gets ONE more try in the same run, after a
//!    wait of minutes ([`retry_delay`]).
//!
//!    Both kinds of waiting, the patient retries inside detection and guidance
//!    and this wait, draw on one run-wide budget ([`crate::retry_budget`],
//!    `CARRICK_RETRY_BUDGET_SECS`, 20 minutes by default). Once it is spent a
//!    service defers after one attempt and the retry is skipped, so a model
//!    that refuses the whole run cannot hold it for hours (carrick#1126).
//! 3. What is still owed after that is PENDING. A pending service that already
//!    has an index is held back, so a thinner index cannot replace it (#461);
//!    every other service lands.
//! 4. A laptop run with pending work does not close its scan with the last
//!    write (`scan_final`), because the cloud stamps a repo's first index
//!    complete on that write and would meter the re-run under the monthly pool
//!    (carrick-cloud#892). It closes it with `scan-failed`, which releases the
//!    slot and stamps nothing.
//!
//! Why the uploads are not moved to "as soon as each service is done": the
//! payloads carry the compat verdicts, SDK edges and boundary the cross-repo
//! join computes from every service, and the cloud answers a second write of a
//! service at the same commit and scanner version `already_current` without
//! storing it. An early write and a later one with verdicts would keep the
//! early one. Rule 1 removes the failure that made the late upload lose work;
//! the design note in the PR that added this module has the rest.

use std::time::Duration;

use crate::agents::framework_guidance_agent::ProtocolGuidance;
use crate::cloud_storage::CloudRepoData;
use crate::framework_detector::DetectionResult;
use crate::scan_health::ServiceLosses;

/// What a service's model stages settled on before its files are analysed.
pub struct ModelSetup {
    pub detection: DetectionResult,
    pub guidance: ProtocolGuidance,
    pub extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
    /// Why this service's model analysis is deferred, when it is: the stage
    /// and the cloud's error code, e.g. `framework detection: model_error`.
    pub deferred: Option<String>,
    /// What a deferred service keeps in its cache because the model did
    /// answer it: the detection and extraction config when only the guidance
    /// failed. Never analysed with (see [`Self::deferred`]).
    kept: Option<KeptAnswers>,
}

/// The answers a guidance-deferred service caches.
struct KeptAnswers {
    detection: DetectionResult,
    extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
}

impl ModelSetup {
    pub fn ready(
        detection: DetectionResult,
        guidance: ProtocolGuidance,
        extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
    ) -> Self {
        Self {
            detection,
            guidance,
            extraction_config,
            deferred: None,
            kept: None,
        }
    }

    /// The setup of a service whose `stage` failed with `error`.
    ///
    /// The detection and guidance it carries are the empty ones local mode
    /// analyses with, and they are never sent anywhere: a deferred service's
    /// file orchestrator dispatches nothing, and [`Self::stamp_cache`] caches
    /// none of it. They exist because the deterministic layer reads the
    /// guidance map's shape and the detection's framework list.
    pub fn deferred(stage: &str, error: &(dyn std::error::Error + 'static)) -> Self {
        let code = error
            .downcast_ref::<crate::agent_service::AgentCallError>()
            .map(|e| e.code.clone())
            .unwrap_or_else(|| "unparseable_response".to_string());
        let again = if crate::retry_budget::remaining().is_zero() {
            "the next scan asks again, because this run has spent its retry budget"
        } else {
            "the scan asks again before it ends"
        };
        tracing::warn!(
            "Deferring this service's model analysis: {stage} failed ({error}). Its files are \
             analysed facts-only for now, and {again}."
        );
        Self {
            detection: DetectionResult::default(),
            guidance: crate::local_mode::offline_guidance(),
            extraction_config: None,
            deferred: Some(format!("{stage}: {code}")),
            kept: None,
        }
    }

    /// The setup of a service whose detection answered and whose guidance
    /// failed with `error`: deferred like [`Self::deferred`], analysed with the
    /// same stand-ins, but the detection (and the extraction config asked with
    /// it) is cached, so the next ask is guidance only (carrick#1126).
    pub fn guidance_deferred(
        detection: DetectionResult,
        extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
        error: &(dyn std::error::Error + 'static),
    ) -> Self {
        Self {
            kept: Some(KeptAnswers {
                detection,
                extraction_config,
            }),
            ..Self::deferred("framework guidance", error)
        }
    }

    /// Write the cache fields this setup is allowed to write.
    ///
    /// A deferred service never writes `cached_guidance`: its absence is what
    /// makes the next scan ask for the model stages again (the incremental
    /// branch reuses them only when both are there), so it doubles as the
    /// blob's "pending model analysis" mark without a field of its own. A
    /// detection that answered is kept beside it, and the next scan asks for
    /// the guidance alone.
    pub fn stamp_cache(&self, data: &mut CloudRepoData) {
        if self.deferred.is_some() {
            data.cached_guidance = None;
            data.cached_detection = self.kept.as_ref().map(|kept| kept.detection.clone());
            data.cached_extraction_config = self
                .kept
                .as_ref()
                .and_then(|kept| kept.extraction_config.clone());
            return;
        }
        data.cached_detection = Some(self.detection.clone());
        data.cached_guidance = Some(self.guidance.clone());
        data.cached_extraction_config = self.extraction_config.clone();
    }
}

/// One service's analysis, and whether its model stages were deferred.
pub struct ServiceAnalysis {
    pub data: CloudRepoData,
    pub deferred: Option<String>,
}

/// The model work one service still owes once its analysis has run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwedWork {
    /// Its detection or guidance was deferred (the reason).
    pub deferred: Option<String>,
    /// Files and intents the model did not answer for.
    pub losses: ServiceLosses,
    /// Calls the quota breaker failed fast while it was being analysed.
    pub quota_aborts: usize,
    /// The retry of this service ended in an error of its own (the error).
    pub retry_error: Option<String>,
}

impl OwedWork {
    pub fn is_empty(&self) -> bool {
        self.deferred.is_none()
            && self.losses.is_empty()
            && self.quota_aborts == 0
            && self.retry_error.is_none()
    }

    /// Whether what is owed makes the service's index thinner than a complete
    /// one. Intents are the exception: a function with no intent was never a
    /// reason to hold an index back, and still is not.
    pub fn thins_the_index(&self) -> bool {
        self.deferred.is_some()
            || self.losses.files > 0
            || self.quota_aborts > 0
            || self.retry_error.is_some()
    }

    /// Whether another try in this run could change anything. A budget that
    /// refused the model will refuse it again a few minutes later.
    pub fn worth_retrying(&self) -> bool {
        !self.is_empty()
            && !self
                .deferred
                .as_deref()
                .is_some_and(|reason| reason.ends_with(crate::agent_service::LLM_DISABLED_CODE))
    }

    /// The parenthesis the summary puts after the service's name.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(reason) = &self.deferred {
            parts.push(reason.clone());
        }
        if self.losses.files > 0 {
            parts.push(format!("{} file(s) not analysed", self.losses.files));
        }
        if self.quota_aborts > 0 {
            parts.push(format!(
                "{} call(s) stopped by the LLM quota",
                self.quota_aborts
            ));
        }
        if self.losses.intents > 0 {
            parts.push(format!(
                "{} function description(s) missing",
                self.losses.intents
            ));
        }
        if let Some(error) = &self.retry_error {
            parts.push(format!("retry failed: {error}"));
        }
        parts.join("; ")
    }
}

/// Whether a service that still owes `owed` is kept out of this run's upload.
///
/// - Nothing owed that thins the index: it lands.
/// - Its retry failed for a reason of its own: held back, because what the
///   run holds for it is from before a failure nobody classified.
/// - Deferred, or stopped by the quota breaker: its files were never sent, so
///   no unanalysed-file list describes the gap. It lands only when there is
///   no index to thin (`has_index` false).
/// - Files the analyzer did not answer for: it lands when the cloud decides
///   with the service's own list (a laptop first index of the service,
///   `laptop && !has_index`). CI never sends the list, so CI holds it back.
///
/// `allow_partial` (`CARRICK_ALLOW_PARTIAL_ANALYSIS`) lands everything but a
/// failed retry.
pub fn holds_back(owed: &OwedWork, has_index: bool, laptop: bool, allow_partial: bool) -> bool {
    if owed.retry_error.is_some() {
        return true;
    }
    if allow_partial {
        return false;
    }
    if owed.deferred.is_some() || owed.quota_aborts > 0 {
        return has_index;
    }
    owed.losses.files > 0 && (has_index || !laptop)
}

/// Set to a number of seconds to change how long a run waits before it
/// retries the work it still owes. Read for tests, which cannot wait minutes;
/// capped at [`MAX_RETRY_DELAY`] so a typo cannot park a scan for a day.
pub const RETRY_DELAY_ENV: &str = "CARRICK_PENDING_RETRY_DELAY_SECS";

/// How long a run waits before its one retry. Minutes, because the failure
/// this retries is a shared model quota that refills on that scale, and the
/// calls themselves already waited up to ten.
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(180);
/// The wait when the only work owed is function descriptions: they thin no
/// index, and a CI job should not idle three minutes for a sentence.
const INTENTS_ONLY_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(900);

/// The wait before the retry. `thinning` is whether any service being retried
/// owes work that thins its index ([`OwedWork::thins_the_index`]).
pub fn retry_delay(thinning: bool) -> Duration {
    retry_delay_from(std::env::var(RETRY_DELAY_ENV).ok().as_deref(), thinning)
}

fn retry_delay_from(value: Option<&str>, thinning: bool) -> Duration {
    let default = if thinning {
        DEFAULT_RETRY_DELAY
    } else {
        INTENTS_ONLY_RETRY_DELAY
    };
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
        .min(MAX_RETRY_DELAY)
}

/// Sleep `delay` as the run's in-run retry wait: charged to the run's retry
/// budget, and saying how long is left at the start and about once a minute,
/// so a run parked here never reads as a dead one.
pub async fn wait_with_progress(delay: Duration, mut progress: impl FnMut(Duration)) {
    const TICK: Duration = Duration::from_secs(60);
    let mut left = delay;
    progress(left);
    while !left.is_zero() {
        let step = left.min(TICK);
        crate::retry_budget::wait(step).await;
        left -= step;
        if !left.is_zero() {
            progress(left);
        }
    }
}

/// The closing lines of a run that still owes work: which services are
/// complete, which are pending and why, and what re-running does.
///
/// `laptop` picks the sentence about what to run: `carrick index` is the
/// command on a laptop, and the next push is what re-scans in CI.
pub fn pending_summary(
    complete: &[String],
    pending: &[(String, OwedWork)],
    laptop: bool,
) -> String {
    let complete_line = if complete.is_empty() {
        "Complete: none.".to_string()
    } else {
        format!("Complete: {}.", complete.join(", "))
    };
    let pending_line = pending
        .iter()
        .map(|(service, owed)| format!("{service} ({})", owed.describe()))
        .collect::<Vec<_>>()
        .join(", ");
    let rerun = if laptop {
        "Re-run `carrick index` to finish them: it asks the model only for the pending work and \
         replays everything else from cache."
    } else {
        "The next scan asks the model only for the pending work and replays everything else \
         from cache."
    };
    format!("{complete_line} Pending model analysis: {pending_line}. {rerun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retry_waits_minutes_unless_told_otherwise_and_never_unbounded() {
        assert_eq!(retry_delay_from(None, true), Duration::from_secs(180));
        assert_eq!(retry_delay_from(None, false), Duration::from_secs(30));
        assert_eq!(retry_delay_from(Some("0"), true), Duration::ZERO);
        assert_eq!(
            retry_delay_from(Some("nonsense"), true),
            Duration::from_secs(180)
        );
        assert_eq!(retry_delay_from(Some("86400"), true), MAX_RETRY_DELAY);
    }

    #[test]
    fn a_service_is_held_back_only_when_it_would_thin_an_index_that_exists() {
        let deferred = OwedWork {
            deferred: Some("framework detection: model_error".to_string()),
            ..OwedWork::default()
        };
        let lost = OwedWork {
            losses: ServiceLosses {
                files: 2,
                intents: 0,
            },
            ..OwedWork::default()
        };
        let intents_only = OwedWork {
            losses: ServiceLosses {
                files: 0,
                intents: 4,
            },
            ..OwedWork::default()
        };
        // First index: facts-only beats nothing, on either path.
        assert!(!holds_back(&deferred, false, true, false));
        assert!(!holds_back(&deferred, false, false, false));
        // An index exists: stale, not thinner.
        assert!(holds_back(&deferred, true, true, false));
        // Lost files: the laptop's first index lets the cloud decide with the
        // list; CI never sends one.
        assert!(!holds_back(&lost, false, true, false));
        assert!(holds_back(&lost, false, false, false));
        assert!(holds_back(&lost, true, true, false));
        // The operator's opt-out, and the one case it does not cover.
        assert!(!holds_back(&lost, true, false, true));
        let failed_retry = OwedWork {
            retry_error: Some("io".to_string()),
            ..OwedWork::default()
        };
        assert!(holds_back(&failed_retry, false, true, true));
        // A missing intent never held an index back.
        assert!(!holds_back(&intents_only, true, false, false));
        assert!(!holds_back(&OwedWork::default(), true, false, false));
    }

    #[test]
    fn a_missing_intent_is_owed_work_but_does_not_thin_the_index() {
        let owed = OwedWork {
            losses: ServiceLosses {
                files: 0,
                intents: 3,
            },
            ..OwedWork::default()
        };
        assert!(!owed.is_empty());
        assert!(!owed.thins_the_index());
        assert!(owed.worth_retrying());
    }

    #[test]
    fn a_budget_refusal_is_pending_but_not_retried_in_the_same_run() {
        let owed = OwedWork {
            deferred: Some(format!(
                "framework detection: {}",
                crate::agent_service::LLM_DISABLED_CODE
            )),
            ..OwedWork::default()
        };
        assert!(owed.thins_the_index());
        assert!(!owed.worth_retrying());
    }

    #[test]
    fn a_guidance_failure_keeps_the_detection_and_analyses_with_the_stand_ins() {
        let error = crate::agent_service::AgentCallError {
            code: "model_error".to_string(),
            message: "overloaded".to_string(),
            retriable: true,
        };
        let detection = DetectionResult {
            frameworks: vec!["express".to_string()],
            ..DetectionResult::default()
        };
        let setup = ModelSetup::guidance_deferred(detection.clone(), None, &error);
        assert_eq!(
            setup.deferred.as_deref(),
            Some("framework guidance: model_error")
        );
        // Analysed with the stand-ins, like any deferred service.
        assert!(setup.detection.frameworks.is_empty());

        // What it caches is asserted end to end in
        // `tests/scan_durability_test.rs`, which reads the uploaded payload.
        assert_eq!(
            setup
                .kept
                .as_ref()
                .map(|kept| kept.detection.frameworks.clone()),
            Some(detection.frameworks)
        );
        assert!(
            ModelSetup::deferred("framework detection", &error)
                .kept
                .is_none()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_wait_says_what_is_left_about_once_a_minute_and_charges_the_budget() {
        let _serial = crate::retry_budget::tests::SERIAL.lock().await;
        crate::retry_budget::reset();
        let mut said = Vec::new();
        wait_with_progress(Duration::from_secs(150), |left| said.push(left.as_secs())).await;
        assert_eq!(said, [150, 90, 30]);
        assert_eq!(crate::retry_budget::spent(), Duration::from_secs(150));

        said.clear();
        wait_with_progress(Duration::ZERO, |left| said.push(left.as_secs())).await;
        assert_eq!(said, [0]);
        crate::retry_budget::reset();
    }

    /// What the blob then caches is asserted end to end in
    /// `tests/scan_durability_test.rs`, which reads the uploaded payload.
    #[test]
    fn a_deferred_setup_names_the_stage_and_the_cloud_code() {
        let error = crate::agent_service::AgentCallError {
            code: "model_error".to_string(),
            message: "overloaded".to_string(),
            retriable: true,
        };
        let setup = ModelSetup::deferred("framework detection", &error);
        assert_eq!(
            setup.deferred.as_deref(),
            Some("framework detection: model_error")
        );
        assert!(setup.extraction_config.is_none());
    }

    #[test]
    fn the_summary_names_both_halves_and_what_rerunning_does() {
        let owed = OwedWork {
            deferred: Some("framework detection: model_error".to_string()),
            ..OwedWork::default()
        };
        let line = pending_summary(
            &["api".to_string(), "worker".to_string()],
            &[("billing".to_string(), owed)],
            true,
        );
        assert!(line.contains("Complete: api, worker."), "{line}");
        assert!(
            line.contains("Pending model analysis: billing (framework detection: model_error)"),
            "{line}"
        );
        assert!(line.contains("carrick index"), "{line}");
        assert!(line.contains("only for the pending work"), "{line}");
    }
}
