//! One run-wide ceiling on the long waits a scan makes against a model that
//! keeps refusing (carrick#1126).
//!
//! Framework detection and guidance retry under
//! [`crate::agent_service::RetryPolicy::PATIENT`], up to ten minutes of sleeping
//! each, and a service they defer is analysed again after the engine's in-run
//! retry wait. Each is bounded; their sum was not. Against a backend that
//! refuses the whole run, a seven-service repo could wait about
//! 7 x 10 min x 2 passes before it finished.
//!
//! Every one of those waits draws on this budget. When it is spent, a patient
//! call still makes its first attempt (a backend that has recovered answers
//! it), but no longer sleeps to try again, so the service defers at once; and
//! the engine skips the in-run retry, leaving what is owed pending. The run
//! then finishes.
//!
//! What is charged is wall-clock time during which at least one budgeted wait
//! is sleeping, not the sum of the sleeps: guidance fires five patient calls
//! at once, and charging each would spend the budget five times faster than a
//! detection failure does. Concurrent waiters that each saw room can overrun
//! the ceiling by at most one wait.
//!
//! The cloud frees a laptop scan's slot after 30 quiet minutes
//! (carrick-cloud#930). No single wait comes near it: a patient sleep is at
//! most two minutes, and the in-run retry wait at most fifteen
//! (`engine::durability`), each followed by a cloud call.
//!
//! Knob: [`BUDGET_ENV`], in seconds. Per-file and per-function calls retry
//! under the standard policy and are not charged: they cost one file, and the
//! run goes on without it.

use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// Seconds of waiting a whole run may spend on patient retries and the in-run
/// retry wait, added together. Default [`DEFAULT_BUDGET`], capped at
/// [`MAX_BUDGET`] so a typo cannot make it unbounded; `0` means no waiting at
/// all (every patient call gets one attempt, and there is no in-run retry).
pub const BUDGET_ENV: &str = "CARRICK_RETRY_BUDGET_SECS";

const DEFAULT_BUDGET: Duration = Duration::from_secs(20 * 60);
const MAX_BUDGET: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
struct Ledger {
    /// Charged time from waits that have finished.
    spent: Duration,
    /// When the current stretch of waiting began, while any wait sleeps.
    since: Option<Instant>,
    /// Budgeted waits sleeping right now.
    waiters: usize,
}

impl Ledger {
    fn spent(&self) -> Duration {
        self.spent + self.since.map_or(Duration::ZERO, |since| since.elapsed())
    }
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    spent: Duration::ZERO,
    since: None,
    waiters: 0,
});

fn ledger() -> std::sync::MutexGuard<'static, Ledger> {
    LEDGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The run's budget, read from [`BUDGET_ENV`].
pub fn budget() -> Duration {
    budget_from(std::env::var(BUDGET_ENV).ok().as_deref())
}

fn budget_from(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BUDGET)
        .min(MAX_BUDGET)
}

/// Start a run's ledger from nothing. The engine calls it as a run opens;
/// tests run several scans in one process.
pub fn reset() {
    *ledger() = Ledger::default();
}

/// Waiting charged so far this run.
pub fn spent() -> Duration {
    ledger().spent()
}

/// What is left of the budget.
pub fn remaining() -> Duration {
    budget().saturating_sub(spent())
}

/// Whether a wait of `wait` still fits in what is left.
pub fn fits(wait: Duration) -> bool {
    spent() + wait <= budget()
}

/// Sleep `duration`, charging the budget for it.
pub async fn wait(duration: Duration) {
    let _charge = Charge::start();
    tokio::time::sleep(duration).await;
}

/// Counts one sleeping wait for as long as it lives, so a wait dropped mid
/// sleep (an interrupted run) stops charging too.
struct Charge;

impl Charge {
    fn start() -> Self {
        let mut ledger = ledger();
        if ledger.waiters == 0 {
            ledger.since = Some(Instant::now());
        }
        ledger.waiters += 1;
        Charge
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        let mut ledger = ledger();
        ledger.waiters = ledger.waiters.saturating_sub(1);
        if ledger.waiters == 0
            && let Some(since) = ledger.since.take()
        {
            ledger.spent += since.elapsed();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The ledger is a process global; tests that touch it hold this.
    pub(crate) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn the_budget_defaults_to_twenty_minutes_and_is_never_unbounded() {
        assert_eq!(budget_from(None), Duration::from_secs(1200));
        assert_eq!(budget_from(Some("0")), Duration::ZERO);
        assert_eq!(budget_from(Some(" 90 ")), Duration::from_secs(90));
        assert_eq!(budget_from(Some("soon")), Duration::from_secs(1200));
        assert_eq!(budget_from(Some("999999")), MAX_BUDGET);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_waits_are_charged_the_time_they_overlap_once() {
        let _serial = SERIAL.lock().await;
        reset();
        tokio::join!(
            wait(Duration::from_secs(100)),
            wait(Duration::from_secs(100)),
            wait(Duration::from_secs(60)),
        );
        assert_eq!(spent(), Duration::from_secs(100));

        // Waits that do not overlap add up.
        wait(Duration::from_secs(30)).await;
        assert_eq!(spent(), Duration::from_secs(130));

        reset();
        assert_eq!(spent(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_dropped_mid_sleep_stops_charging() {
        let _serial = SERIAL.lock().await;
        reset();
        let _ = tokio::time::timeout(Duration::from_secs(10), wait(Duration::from_secs(100))).await;
        tokio::time::advance(Duration::from_secs(50)).await;
        assert_eq!(spent(), Duration::from_secs(10));
        reset();
    }
}
