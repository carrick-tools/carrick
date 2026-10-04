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
//!    service, files the analyzer never answered for, intents that failed —
//!    gets ONE more try in the same run. The try comes after a wait of
//!    minutes when the model refused any of it for capacity, and at once when
//!    it refused none ([`retry_delay`], carrick#1896): a wait mends a refusal
//!    and nothing else. What a limit refused is not retried: it would be
//!    refused again.
//!
//!    Both kinds of waiting, the patient retries inside detection and guidance
//!    and this wait, draw on one run-wide budget ([`crate::retry_budget`],
//!    `CARRICK_RETRY_BUDGET_SECS`, 20 minutes by default). Once it is spent a
//!    service defers after one attempt and the retry is skipped, so a model
//!    that refuses the whole run cannot hold it for hours (carrick#1126).
//! 3. What is still owed after that is PENDING. A pending service that already
//!    has an index is held back, so a thinner index cannot replace it (#461);
//!    every other service lands. On CI a pending service fails the run, unless
//!    all it owes is what a limit refused: hitting a limit never fails a scan
//!    (carrick-cloud#401).
//! 4. A laptop run with pending work still closes its scan with the last write
//!    (`scan_final`), and that write names the pending services
//!    (`pending_services`), so the cloud releases the slot, writes the receipt
//!    and the last-scan row, and leaves the repo's first index open for the
//!    re-run (carrick-cloud#892). A cloud that did not answer
//!    `accepts_pending_services` at `start-scan` would stamp the first index
//!    complete on that write whatever it said, so against such a cloud, or when
//!    no write is left to carry the list, no write is marked final and the run
//!    closes with `scan-failed`, which releases the slot and stamps nothing.
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
    /// The retry of this service ended in an error of its own (the error).
    pub retry_error: Option<String>,
}

impl OwedWork {
    pub fn is_empty(&self) -> bool {
        self.deferred.is_none() && self.losses.is_empty() && self.retry_error.is_none()
    }

    /// Whether what is owed makes the service's index thinner than a complete
    /// one. Intents are the exception: a function with no intent was never a
    /// reason to hold an index back, and still is not.
    pub fn thins_the_index(&self) -> bool {
        self.deferred.is_some() || self.losses.files > 0 || self.retry_error.is_some()
    }

    /// Whether its model stages were deferred because a limit of the cloud's
    /// refused them (`llm_disabled`), rather than because the model did not
    /// answer.
    fn deferred_by_refusal(&self) -> bool {
        self.deferred
            .as_deref()
            .is_some_and(|reason| reason.ends_with(crate::agent_service::LLM_DISABLED_CODE))
    }

    /// Whether everything that thins this service's index is what a limit
    /// refused. Such a service is still pending, and still held back when it
    /// has an index, but it does not fail a CI run: hitting a limit never
    /// fails a scan (carrick-cloud#401).
    pub fn only_refused(&self) -> bool {
        self.deferred_by_refusal() && self.losses.files == 0 && self.retry_error.is_none()
    }

    /// Whether another try in this run could change anything. A budget that
    /// refused the model will refuse it again a few minutes later.
    pub fn worth_retrying(&self) -> bool {
        !self.is_empty() && !self.deferred_by_refusal()
    }

    /// What of this the model refused for capacity
    /// ([`crate::agent_service::CAPACITY_REFUSAL_CODE`]), which is what a wait
    /// before the retry is for: a deferral that ended on one, a file, or an
    /// intent. `None` when it refused nothing here, whatever else is owed. A
    /// request the gateway cut is owed too, and its answer is either in the
    /// cloud already or still being computed there; neither is nearer for
    /// three idle minutes.
    ///
    /// Not the refusal [`Self::only_refused`] reads: that one is a limit of
    /// the cloud's saying no (`llm_disabled`), which no wait in this run
    /// changes and which is never retried.
    pub fn refused_for_capacity(&self) -> Option<RefusedForCapacity> {
        let deferred = self
            .deferred
            .as_deref()
            .is_some_and(|reason| reason.ends_with(crate::agent_service::CAPACITY_REFUSAL_CODE));
        if deferred || self.losses.capacity_files > 0 {
            Some(RefusedForCapacity::Thinning)
        } else if self.losses.capacity_intents > 0 {
            Some(RefusedForCapacity::Intents)
        } else {
            None
        }
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
/// - Deferred: its files were never sent, so no unanalysed-file list describes
///   the gap. It lands only when there is no index to thin (`has_index`
///   false).
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
    if owed.deferred.is_some() {
        return has_index;
    }
    owed.losses.files > 0 && (has_index || !laptop)
}

/// Set to a number of seconds to change how long a run waits before it
/// retries owed work the model refused for capacity. It sets the length of
/// that wait and never adds one: a run that owes nothing of the kind does not
/// wait. Read for tests, which cannot wait minutes; capped at
/// [`MAX_RETRY_DELAY`] so a typo cannot park a scan for a day.
pub const RETRY_DELAY_ENV: &str = "CARRICK_PENDING_RETRY_DELAY_SECS";

/// How long a run waits before its one retry of work the model refused for
/// capacity. Minutes, because that is a shared model quota that refills on
/// that scale, and the calls themselves already waited up to ten.
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(180);
/// The wait when the only work the model refused is function descriptions:
/// they thin no index, and a CI job should not idle three minutes for a
/// sentence.
const INTENTS_ONLY_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(900);

/// The owed work a model refused for capacity, by what its absence costs.
/// Ordered, so the most a set of services was refused is its `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefusedForCapacity {
    /// Function descriptions only.
    Intents,
    /// A file, or a service's model stages: the index is thinner without it.
    Thinning,
}

/// The wait before the retry, given the most the model refused any service
/// being retried ([`OwedWork::refused_for_capacity`]). Nothing refused is no
/// wait at all, whatever [`RETRY_DELAY_ENV`] says (carrick#1896): on one large
/// first index the run idled three minutes before asking again for eight
/// files, six of which a gateway cut had ended and four of which the cloud
/// already held the answer to.
pub fn retry_delay(refused: Option<RefusedForCapacity>) -> Duration {
    retry_delay_from(std::env::var(RETRY_DELAY_ENV).ok().as_deref(), refused)
}

fn retry_delay_from(value: Option<&str>, refused: Option<RefusedForCapacity>) -> Duration {
    let default = match refused {
        None => return Duration::ZERO,
        Some(RefusedForCapacity::Thinning) => DEFAULT_RETRY_DELAY,
        Some(RefusedForCapacity::Intents) => INTENTS_ONLY_RETRY_DELAY,
    };
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
        .min(MAX_RETRY_DELAY)
}

/// The line the run's log gets as the retry starts: how many services, how
/// long it waits first, and why. Written because the wait was once a constant
/// and is now a decision, and a log that does not state a decision leaves the
/// next reader to infer it.
pub fn retry_wait_line(
    services: usize,
    delay: Duration,
    refused: Option<RefusedForCapacity>,
) -> String {
    let what = match refused {
        None => {
            return format!(
                "Retrying {services} service(s) now: nothing they owe was refused for capacity, \
                 so there is nothing to wait for"
            );
        }
        Some(RefusedForCapacity::Thinning) => "a file or a service's setup",
        Some(RefusedForCapacity::Intents) => "function descriptions and nothing else",
    };
    format!(
        "Retrying {services} service(s) in {}s: the model refused {what} for capacity",
        delay.as_secs()
    )
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

    /// What the model refused for capacity waits minutes, or half a minute
    /// when it is only function descriptions, unless told otherwise and never
    /// unbounded.
    #[test]
    fn the_retry_waits_minutes_unless_told_otherwise_and_never_unbounded() {
        let thinning = Some(RefusedForCapacity::Thinning);
        let intents = Some(RefusedForCapacity::Intents);
        assert_eq!(retry_delay_from(None, thinning), Duration::from_secs(180));
        assert_eq!(retry_delay_from(None, intents), Duration::from_secs(30));
        assert_eq!(retry_delay_from(Some("0"), thinning), Duration::ZERO);
        assert_eq!(
            retry_delay_from(Some("nonsense"), thinning),
            Duration::from_secs(180)
        );
        assert_eq!(retry_delay_from(Some("86400"), thinning), MAX_RETRY_DELAY);
    }

    /// carrick#1896: with nothing refused for capacity there is nothing a wait
    /// mends, so the retry starts at once, and no setting makes it wait.
    #[test]
    fn the_retry_does_not_wait_when_nothing_owed_was_refused_for_capacity() {
        assert_eq!(retry_delay_from(None, None), Duration::ZERO);
        assert_eq!(retry_delay_from(Some("600"), None), Duration::ZERO);
    }

    fn lost(files: usize, capacity_files: usize) -> OwedWork {
        OwedWork {
            losses: ServiceLosses {
                files,
                capacity_files,
                ..ServiceLosses::default()
            },
            ..OwedWork::default()
        }
    }

    fn deferred_on(code: &str) -> OwedWork {
        OwedWork {
            deferred: Some(format!("framework detection: {code}")),
            ..OwedWork::default()
        }
    }

    /// What decides the wait is why each thing is owed, not how much is. The
    /// shape is the one large first index the rule was written from: eight
    /// files owed by four services, six ended by a gateway cut and two by the
    /// model refusing. Those two make the run wait; once a refusal no longer
    /// ends a file, the same eight owe no wait at all.
    #[test]
    fn only_a_capacity_refusal_among_the_owed_work_makes_the_retry_wait() {
        let cut = [lost(1, 0), lost(5, 0), lost(1, 0), lost(1, 0)];
        let most = |owed: &[OwedWork]| owed.iter().filter_map(OwedWork::refused_for_capacity).max();
        assert_eq!(most(&cut), None);
        assert_eq!(retry_delay_from(None, most(&cut)), Duration::ZERO);

        let as_logged = [lost(1, 0), lost(5, 1), lost(1, 1), lost(1, 0)];
        assert_eq!(most(&as_logged), Some(RefusedForCapacity::Thinning));
        assert_eq!(
            retry_delay_from(None, most(&as_logged)),
            Duration::from_secs(180)
        );

        // A deferral is read by the code it ended on: the model refusing
        // waits, and anything else that deferred a service does not.
        assert_eq!(
            deferred_on(crate::agent_service::CAPACITY_REFUSAL_CODE).refused_for_capacity(),
            Some(RefusedForCapacity::Thinning)
        );
        assert_eq!(deferred_on("gateway_error").refused_for_capacity(), None);
        assert_eq!(
            deferred_on("unparseable_response").refused_for_capacity(),
            None
        );

        // Intents the model refused wait the short wait, and only when nothing
        // that thins an index was refused beside them.
        let intents = OwedWork {
            losses: ServiceLosses {
                intents: 4,
                capacity_intents: 1,
                ..ServiceLosses::default()
            },
            ..OwedWork::default()
        };
        assert_eq!(
            intents.refused_for_capacity(),
            Some(RefusedForCapacity::Intents)
        );
        assert_eq!(
            most(&[intents.clone(), lost(2, 0)]),
            Some(RefusedForCapacity::Intents),
            "a cut file beside refused intents does not lengthen the wait"
        );
        assert_eq!(
            most(&[intents, lost(2, 1)]),
            Some(RefusedForCapacity::Thinning)
        );
        // An intent that came back empty is owed and was not refused.
        let discarded = OwedWork {
            losses: ServiceLosses {
                intents: 3,
                ..ServiceLosses::default()
            },
            ..OwedWork::default()
        };
        assert!(discarded.worth_retrying());
        assert_eq!(discarded.refused_for_capacity(), None);
    }

    /// The log says what was decided and why, in a line short enough to read.
    #[test]
    fn the_retry_line_says_how_long_it_waits_and_why() {
        let now = retry_wait_line(4, Duration::ZERO, None);
        assert_eq!(
            now,
            "Retrying 4 service(s) now: nothing they owe was refused for capacity, so there is \
             nothing to wait for"
        );
        let waiting = retry_wait_line(
            2,
            Duration::from_secs(180),
            Some(RefusedForCapacity::Thinning),
        );
        assert_eq!(
            waiting,
            "Retrying 2 service(s) in 180s: the model refused a file or a service's setup for \
             capacity"
        );
        let intents = retry_wait_line(
            1,
            Duration::from_secs(30),
            Some(RefusedForCapacity::Intents),
        );
        assert_eq!(
            intents,
            "Retrying 1 service(s) in 30s: the model refused function descriptions and nothing \
             else for capacity"
        );
        for line in [now, waiting, intents] {
            assert!(line.split_whitespace().count() <= 20, "{line}");
        }
    }

    #[test]
    fn a_service_is_held_back_only_when_it_would_thin_an_index_that_exists() {
        let deferred = OwedWork {
            deferred: Some("framework detection: model_error".to_string()),
            ..OwedWork::default()
        };
        let lost = lost(2, 0);
        let intents_only = OwedWork {
            losses: ServiceLosses {
                intents: 4,
                ..ServiceLosses::default()
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
                intents: 3,
                ..ServiceLosses::default()
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
        // Pending, and it fails no run.
        assert!(owed.only_refused());
        // A deferral the model did not answer is not a refusal, and a refused
        // service that also lost a file owes a loss as well.
        let unanswered = OwedWork {
            deferred: Some("framework detection: model_error".to_string()),
            ..OwedWork::default()
        };
        assert!(!unanswered.only_refused());
        let refused_and_lost = OwedWork {
            losses: ServiceLosses {
                files: 1,
                ..ServiceLosses::default()
            },
            ..owed.clone()
        };
        assert!(!refused_and_lost.only_refused());
        assert!(!OwedWork::default().only_refused());
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
