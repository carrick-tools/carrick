//! The time limits that ran out during a scan, and what each one cost
//! (carrick#2021).
//!
//! A time limit that runs out does not fail the scan. The work it did not
//! reach comes back with a reason, the scan carries on, and the index is
//! thinner than it looks: a retype that ran out leaves response checks
//! `unverifiable`, a library check that ran out drops a claim. Until this
//! module nothing added those reasons up, so a cut-off was invisible unless
//! somebody read the log. Each limit is counted by kind, in the unit of what
//! it cost, stated in the scan summary and in each service's boundary, and
//! written to the index with the boundary.
//!
//! Each count reads the reason the limit already leaves on what it cut off:
//!
//! | Limit | Where it is set | The reason it leaves | Counted where |
//! |---|---|---|---|
//! | Retype | the scan's one retype ceiling, `RETYPE_SCAN_CEILING` in `engine/type_compat_v2.rs`, and a request's `RETYPE_BUDGET_MS` in the sidecar's `index.ts` | an abstain saying "the retype check ran out of its N ms budget" (the ceiling's adds "for the whole scan"), on the pair's unresolved reason | [`from_check_outcomes`] |
//! | Library checks | `SEMANTICS_BUDGET_MS` in the sidecar's `index.ts` | a verdict `unchecked` with reason `budget` | the two places a service's library checks are answered |
//! | Sidecar deadline | `OPERATION_TIMEOUT` in `services/type_sidecar.rs` | the operation fails with a timeout and the process is replaced | the sidecar client while a service is analysed; [`from_check_outcomes`] in the cross-service check |
//!
//! The fourth limit, the unwidened reading's `UNWIDENED_BUDGET_MS` in the
//! sidecar's `type-inferrer.ts`, is not counted: the sidecar only logs that
//! it ran out, and an inference without an unwidened reading looks the same
//! to the scanner whether the limit ran out or the reading was never owed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analyzer::PairCheckOutcome;
use crate::services::type_sidecar::SidecarError;

/// A time limit that ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeLimit {
    /// The retype check's time limit: consumer calls it did not reach.
    Retype,
    /// The library checks' time limit: checks the sidecar answered
    /// `unchecked` with reason `budget`.
    LibraryChecks,
    /// The sidecar went silent for the whole operation deadline: the items
    /// the requests it never answered carried.
    SidecarDeadline,
}

/// What the time limits that ran out cost one service, by kind (carrick#2021).
///
/// On the index as the boundary's `time_limits_run_out`. A kind that did not
/// run out is left off the wire, so a scan where nothing ran out writes `{}`,
/// and a blob without the block at all is from a scanner that did not count.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeLimitsRunOut {
    /// Response checks (a consumer call retyped with the producer's response
    /// type) the retype time limit did not reach. Each is `unverifiable`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retype: usize,
    /// Client-library and library-claim checks the sidecar did not reach in
    /// its time limit. Each claim is dropped for this scan.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub library_checks: usize,
    /// Items carried by sidecar requests that went the whole operation
    /// deadline without an answer or a sign of life: type readings, captured
    /// types, response checks, library checks. A timed-out request is not
    /// asked again, so none of them was answered.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sidecar_deadline: usize,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

impl TimeLimitsRunOut {
    /// Count `cost` more against `limit`.
    pub fn add(&mut self, limit: TimeLimit, cost: usize) {
        let count = match limit {
            TimeLimit::Retype => &mut self.retype,
            TimeLimit::LibraryChecks => &mut self.library_checks,
            TimeLimit::SidecarDeadline => &mut self.sidecar_deadline,
        };
        *count += cost;
    }

    /// Add every count of `other` to this one.
    pub fn merge(&mut self, other: &TimeLimitsRunOut) {
        self.add(TimeLimit::Retype, other.retype);
        self.add(TimeLimit::LibraryChecks, other.library_checks);
        self.add(TimeLimit::SidecarDeadline, other.sidecar_deadline);
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line per limit that ran out, naming what it skipped and why, and
    /// nothing for a limit that did not. The scan summary prints the run's
    /// total; each service's boundary prints its own.
    pub fn lines(&self) -> Vec<String> {
        let deadline_minutes = crate::services::type_sidecar::OPERATION_TIMEOUT.as_secs() / 60;
        [
            (
                self.retype,
                "response check",
                "not reached: retype time limit".to_string(),
            ),
            (
                self.library_checks,
                "library check",
                "not reached: library check time limit".to_string(),
            ),
            (
                self.sidecar_deadline,
                "type lookup",
                format!("not answered: the type checker was silent for {deadline_minutes} minutes"),
            ),
        ]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, noun, why)| format!("{} {why}", crate::scan_timing::plural(count, noun)))
        .collect()
    }
}

/// The words both retype limits use for an item they did not reach: the
/// scan's ceiling for a request it did not send (`retype_service`), and the
/// sidecar's for an item inside a request it did not get to (`retype.ts`).
/// Each continues with the limit in milliseconds and `ms budget`.
const RETYPE_RAN_OUT: &str = "the retype check ran out of its ";

/// What the time limits cost each consumer service in the cross-service type
/// check, read off the check's outcomes. Keyed by the consumer's service id
/// (`service_name ?? repo_name`), the service whose blob stores the verdict.
///
/// Read off the outcomes rather than counted where the check runs, because
/// the outcome is where every cut-off ends up: a retype item a limit did not
/// reach, and a pair whose request the sidecar never answered, both leave the
/// pair unresolved with the reason appended. A pair is counted once per kind
/// whose reason it carries.
pub fn from_check_outcomes(outcomes: &[PairCheckOutcome]) -> BTreeMap<String, TimeLimitsRunOut> {
    let silent = SidecarError::Timeout.to_string();
    let mut by_consumer: BTreeMap<String, TimeLimitsRunOut> = BTreeMap::new();
    for outcome in outcomes {
        let Some(reason) = outcome.unresolved_reason.as_deref() else {
            continue;
        };
        let mut cost = TimeLimitsRunOut::default();
        if reason.contains(RETYPE_RAN_OUT) {
            cost.add(TimeLimit::Retype, 1);
        }
        if reason.contains(&silent) {
            cost.add(TimeLimit::SidecarDeadline, 1);
        }
        if !cost.is_empty() {
            by_consumer
                .entry(outcome.consumer_service.clone())
                .or_default()
                .merge(&cost);
        }
    }
    by_consumer
}

/// Fold what the cross-service check's time limits cost into each local
/// service's boundary (keyed by service id), and return the run's total:
/// what the scan summary states. Every boundary comes out counted, `{}` when
/// no limit cost it anything.
pub fn fold_check_outcomes(
    boundaries: &mut [(String, crate::boundary::ServiceBoundary)],
    outcomes: &[PairCheckOutcome],
) -> TimeLimitsRunOut {
    let checked = from_check_outcomes(outcomes);
    let mut total = TimeLimitsRunOut::default();
    for (id, boundary) in boundaries.iter_mut() {
        boundary.fold_time_limits(checked.get(id).unwrap_or(&TimeLimitsRunOut::default()));
        total.merge(&boundary.time_limits_run_out.unwrap_or_default());
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::ManifestTypeKind;
    use crate::services::type_sidecar::VerdictBucket;

    fn outcome(consumer: &str, reason: Option<&str>) -> PairCheckOutcome {
        PairCheckOutcome {
            pair_key: format!("api/Res~{consumer}/Call"),
            pseudo_method: "GET".to_string(),
            identity: "/orders".to_string(),
            consumer_file: "src/client.ts".to_string(),
            consumer_line: 1,
            type_kind: ManifestTypeKind::Response,
            bucket: VerdictBucket::Unverifiable,
            gate: None,
            diagnostic: reason.map(str::to_string),
            producer_alias: "Res".to_string(),
            consumer_alias: "Call".to_string(),
            producer_service: "api".to_string(),
            consumer_service: consumer.to_string(),
            resolved: reason.is_none(),
            unresolved_reason: reason.map(str::to_string),
            notes: Vec::new(),
            consumer_reads: Vec::new(),
        }
    }

    /// carrick#2021: both retype limits leave the same sentence, the
    /// scanner's for a batch it did not send and the sidecar's for an item it
    /// did not reach, and the retype appends it to the pair's reason. Each
    /// such pair is one response check its consumer did not get. A pair the
    /// sidecar never answered counts against the deadline; a pair left
    /// unresolved for any other reason, or resolved, counts against nothing.
    #[test]
    fn the_check_outcomes_count_each_cut_off_against_its_consumer() {
        let outcomes = vec![
            // What the scanner writes for a batch it did not send.
            outcome(
                "web",
                Some(
                    "retyping the consumer's call did not decide it: the retype check ran out \
                     of its 600000ms budget",
                ),
            ),
            // What the sidecar writes for an item it did not reach, behind
            // the check's own reason.
            outcome(
                "web",
                Some(
                    "the producer's type has `any`; retyping the consumer's call did not decide \
                     it: the retype check ran out of its 412000ms budget",
                ),
            ),
            outcome(
                "admin",
                Some(
                    "retyping the consumer's call did not decide it: the retype check did not \
                     run: Sidecar operation timed out",
                ),
            ),
            outcome(
                "admin",
                Some("type check did not run: Sidecar operation timed out"),
            ),
            outcome(
                "admin",
                Some("retyping the consumer's call did not decide it: no call at the locator"),
            ),
            outcome("web", None),
        ];

        let by_consumer = from_check_outcomes(&outcomes);
        assert_eq!(
            by_consumer,
            BTreeMap::from([
                (
                    "web".to_string(),
                    TimeLimitsRunOut {
                        retype: 2,
                        ..Default::default()
                    }
                ),
                (
                    "admin".to_string(),
                    TimeLimitsRunOut {
                        sidecar_deadline: 2,
                        ..Default::default()
                    }
                ),
            ])
        );
        assert!(from_check_outcomes(&[outcome("web", None)]).is_empty());
    }

    /// The sentence counted above is the one the sidecar writes. A change to
    /// either side's wording that the other does not follow stops the count,
    /// so the sidecar's source is read here.
    #[test]
    fn the_retype_sentence_is_the_sidecars_own() {
        let retype_ts = include_str!("sidecar/src/retype.ts");
        assert!(
            retype_ts.contains(&format!("`{RETYPE_RAN_OUT}${{budgetMs}}ms budget`")),
            "the sidecar's retype no longer says \"{RETYPE_RAN_OUT}<N>ms budget\""
        );
    }

    /// One line per limit that ran out, in the scan summary and in the
    /// boundary alike, and nothing when none did.
    #[test]
    fn each_limit_that_ran_out_is_one_line_naming_what_it_skipped() {
        assert!(TimeLimitsRunOut::default().lines().is_empty());

        let mut cost = TimeLimitsRunOut::default();
        cost.add(TimeLimit::Retype, 312);
        cost.add(TimeLimit::LibraryChecks, 1);
        cost.add(TimeLimit::SidecarDeadline, 50);
        assert_eq!(
            cost.lines(),
            vec![
                "312 response checks not reached: retype time limit",
                "1 library check not reached: library check time limit",
                "50 type lookups not answered: the type checker was silent for 15 minutes",
            ]
        );
    }

    /// The wire spelling the index carries, and a kind that did not run out
    /// left off it.
    #[test]
    fn a_count_is_written_by_kind_and_a_kind_that_did_not_run_out_is_left_off() {
        let cost = TimeLimitsRunOut {
            retype: 312,
            sidecar_deadline: 50,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&cost).unwrap(),
            r#"{"retype":312,"sidecar_deadline":50}"#
        );
        assert_eq!(
            serde_json::to_string(&TimeLimitsRunOut::default()).unwrap(),
            "{}"
        );
        let read: TimeLimitsRunOut = serde_json::from_str(r#"{"library_checks":4}"#).unwrap();
        assert_eq!(
            read,
            TimeLimitsRunOut {
                library_checks: 4,
                ..Default::default()
            }
        );
    }
}
