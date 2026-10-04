//! How long a run's per-file calls wait out a route that refuses for capacity
//! (carrick#1893).
//!
//! A model on shared quota refuses in windows: for minutes at a time a share
//! of the requests on a route come back 503 `model_error`, and then it stops.
//! Counting attempts is the wrong patience for that. A file's seven attempts
//! fit inside one window (about two minutes of backoff), so the file was given
//! up on while the requests sent a minute later were answered. On one large
//! first index the longest window ran twelve and a half minutes, and files
//! were lost in it that way.
//!
//! So a capacity refusal costs a call no attempt. What it draws on instead is
//! this: one ledger per route, charged for the wall-clock time during which at
//! least one call of that route is waiting on a refusal, from the refusal
//! until that call's next answer. Overlapping waits are charged once, which
//! makes the ledger a ceiling on how much longer the SCAN runs, not a sum over
//! its files. While the ledger has room
//! ([`crate::agent_service::RetryPolicy`]'s refusal budget), a refused call
//! sleeps and asks again; once it is spent, a refusal costs an attempt, as
//! every other retriable error does, so a route that refuses the whole run
//! still ends every call.
//!
//! Kept beside the limiter rather than in it. The limiter decides how many
//! requests a refused route carries at once; this decides how long a call
//! keeps asking. Neither reads the other.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::retry_budget::{Charge, Ledger};

/// One call's open wait on its route's refusal. The route's ledger is charged
/// until it drops.
pub(crate) type Waiting = Charge<Arc<Ledger>>;

/// How much refused time passes between two terminal lines about it. The
/// limiter's own line says the route is slow when its limit moves, and a
/// limit at its floor does not move: without this a long window is a scan
/// that prints nothing.
const WAITING_LINE_EVERY: Duration = Duration::from_secs(5 * 60);

/// One route's refused time in this run.
#[derive(Debug)]
pub(crate) struct RouteRefusal {
    route: String,
    ledger: Arc<Ledger>,
    /// How many [`WAITING_LINE_EVERY`] steps of refused time the terminal
    /// has heard about.
    said_steps: AtomicU64,
    /// Whether the terminal has heard that the budget is spent.
    said_spent: AtomicBool,
}

impl RouteRefusal {
    fn new(route: &str) -> Self {
        Self {
            route: route.to_string(),
            ledger: Arc::new(Ledger::new()),
            said_steps: AtomicU64::new(0),
            said_spent: AtomicBool::new(false),
        }
    }

    /// How long this run's calls on the route have waited on refusals,
    /// overlapping waits counted once.
    pub(crate) fn spent(&self) -> Duration {
        self.ledger.spent()
    }

    /// Whether a wait of `next` still fits in `budget`.
    pub(crate) fn fits(&self, next: Duration, budget: Duration) -> bool {
        self.spent() + next <= budget
    }

    /// Open a wait: the route's ledger is charged until the guard drops.
    pub(crate) fn wait(&self) -> Waiting {
        Ledger::open(Arc::clone(&self.ledger))
    }

    /// Say how long the route has been refused, once per
    /// [`WAITING_LINE_EVERY`] of refused time, and how long its calls go on
    /// asking.
    pub(crate) fn say_waiting(&self, budget: Duration) {
        let steps = self.spent().as_secs() / WAITING_LINE_EVERY.as_secs();
        if steps == 0 || self.said_steps.fetch_max(steps, Ordering::Relaxed) >= steps {
            return;
        }
        crate::progress::announce(&waiting_line(
            &self.route,
            WAITING_LINE_EVERY * steps as u32,
            budget,
        ));
    }

    /// Say, once a run, that the route's budget is spent and what a refusal
    /// costs from here on. Nothing is said for a policy that never had one.
    pub(crate) fn say_spent(&self, budget: Duration, attempts: u32) {
        if budget.is_zero() || self.said_spent.swap(true, Ordering::Relaxed) {
            return;
        }
        crate::progress::announce(&spent_line(&self.route, budget, attempts));
    }
}

/// The line a run prints while a route's refused calls are waiting.
pub(crate) fn waiting_line(route: &str, refused_for: Duration, budget: Duration) -> String {
    format!(
        "model busy: {} has been refused for {} minutes of this scan; refused calls keep asking \
         for up to {}",
        route.trim_start_matches('/'),
        refused_for.as_secs() / 60,
        budget.as_secs() / 60
    )
}

/// The line a run prints when a route's refusal budget is spent.
pub(crate) fn spent_line(route: &str, budget: Duration, attempts: u32) -> String {
    format!(
        "model busy: {} has been refused for {} minutes of this scan; a call refused from now on \
         is given up after {} attempts",
        route.trim_start_matches('/'),
        budget.as_secs() / 60,
        attempts
    )
}

/// Every route's refused time, created on first use.
#[derive(Debug, Default)]
pub(crate) struct RouteRefusals {
    routes: Mutex<BTreeMap<String, Arc<RouteRefusal>>>,
}

impl RouteRefusals {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The ledger for `route`: the grain the limiter keeps its limits at, for
    /// the same reason. One busy model must not spend another's patience.
    pub(crate) fn for_route(&self, route: &str) -> Arc<RouteRefusal> {
        let mut routes = self.routes.lock().unwrap();
        Arc::clone(
            routes
                .entry(route.to_string())
                .or_insert_with(|| Arc::new(RouteRefusal::new(route))),
        )
    }

    /// Start every route from nothing, as a run opens.
    pub(crate) fn reset(&self) {
        self.routes.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_secs(600);

    /// Twenty files refused together for five minutes cost the scan five
    /// minutes, not a hundred: the ledger charges the time the route was
    /// refused, once.
    #[tokio::test(start_paused = true)]
    async fn overlapping_waits_on_one_route_are_charged_once() {
        let route = RouteRefusals::new().for_route("/analyze-file");
        let waits: Vec<Waiting> = (0..20).map(|_| route.wait()).collect();
        tokio::time::sleep(Duration::from_secs(300)).await;
        drop(waits);
        assert_eq!(route.spent(), Duration::from_secs(300));
        assert!(route.fits(Duration::from_secs(300), BUDGET));
        assert!(!route.fits(Duration::from_secs(301), BUDGET));
    }

    /// Time with no call waiting on a refusal is not charged, so two windows
    /// cost their own lengths and nothing for the quiet between them.
    #[tokio::test(start_paused = true)]
    async fn only_the_time_a_call_is_waiting_is_charged() {
        let route = RouteRefusals::new().for_route("/analyze-file");
        let first = route.wait();
        tokio::time::sleep(Duration::from_secs(120)).await;
        drop(first);
        tokio::time::sleep(Duration::from_secs(900)).await;
        let second = route.wait();
        tokio::time::sleep(Duration::from_secs(60)).await;
        // The open stretch counts while it is open.
        assert_eq!(route.spent(), Duration::from_secs(180));
        drop(second);
        assert_eq!(route.spent(), Duration::from_secs(180));
    }

    /// The limiter's grain: a busy model spends its own route's budget.
    #[tokio::test(start_paused = true)]
    async fn each_route_has_its_own_ledger_and_a_run_starts_them_from_nothing() {
        let routes = RouteRefusals::new();
        let files = routes.for_route("/analyze-file");
        let waiting = files.wait();
        tokio::time::sleep(BUDGET).await;
        drop(waiting);
        assert!(!files.fits(Duration::from_secs(1), BUDGET));
        assert!(
            routes
                .for_route("/generate-intent")
                .fits(Duration::from_secs(1), BUDGET)
        );
        // The same route, asked for twice, is one ledger.
        assert_eq!(routes.for_route("/analyze-file").spent(), BUDGET);

        routes.reset();
        assert_eq!(routes.for_route("/analyze-file").spent(), Duration::ZERO);
    }

    /// Each line says how long, which route, and what happens next, in about
    /// twenty words: a person watching a scan cannot act on more.
    #[test]
    fn the_lines_say_how_long_which_route_and_what_follows() {
        let budget = Duration::from_secs(45 * 60);
        let waiting = waiting_line("/analyze-file", Duration::from_secs(10 * 60), budget);
        assert_eq!(
            waiting,
            "model busy: analyze-file has been refused for 10 minutes of this scan; refused \
             calls keep asking for up to 45"
        );
        let spent = spent_line("/analyze-file", budget, 7);
        assert_eq!(
            spent,
            "model busy: analyze-file has been refused for 45 minutes of this scan; a call \
             refused from now on is given up after 7 attempts"
        );
        for line in [waiting, spent] {
            assert!(line.split_whitespace().count() <= 25, "{line}");
        }
    }

    /// A window the limiter has nothing new to say about still prints: one
    /// line for every five minutes the route has been refused, and no more.
    #[tokio::test(start_paused = true)]
    async fn the_waiting_line_is_due_once_per_five_refused_minutes() {
        let route = RouteRefusals::new().for_route("/analyze-file");
        let due = |route: &RouteRefusal| {
            let before = route.said_steps.load(Ordering::Relaxed);
            route.say_waiting(BUDGET);
            route.said_steps.load(Ordering::Relaxed) > before
        };
        let waiting = route.wait();
        tokio::time::sleep(Duration::from_secs(299)).await;
        assert!(!due(&route));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(due(&route));
        assert!(!due(&route), "said twice for one step");
        tokio::time::sleep(Duration::from_secs(700)).await;
        assert!(due(&route));
        assert_eq!(route.said_steps.load(Ordering::Relaxed), 3);
        drop(waiting);
    }
}
