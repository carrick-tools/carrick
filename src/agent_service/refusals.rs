//! How long a run's per-file calls wait out a route that refuses for capacity
//! (carrick#1893), and what happens on the route once that wait is used up
//! (carrick#1909).
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
//! sleeps and asks again.
//!
//! # Once the budget is spent
//!
//! A route is SILENT when its budget is spent and the last thing its model
//! did, for any call of the run, was refuse. So "the route has answered
//! nothing since" means since its last refusal. On a silent route:
//!
//! - One request at a time is the PROBE ([`RouteRefusal::turn`]). A call that
//!   finds a probe out waits for what comes of it and sends nothing.
//! - A refusal ends the call that was refused ([`RouteRefusal::refused`]),
//!   and decides for the calls behind it: every call waiting on the probe,
//!   and every call that arrives while the refusal still stands, ends WITHOUT
//!   a request of its own. Each is owed for capacity, so the run's retry of
//!   owed work asks again, and so does the next scan. When the refusal no
//!   longer stands, the next call is the probe.
//! - One answer from the model ends the silence at once, and every call is
//!   asked as it always was. The first refusal after it is counted as every
//!   other retriable error is (an attempt, a sleep, another request), and it
//!   makes the route silent again.
//! - A probe that comes to neither (the gateway cut it, the connection was
//!   lost, the cloud said an earlier request for it is still running) says
//!   nothing about the model, and the next call takes the turn.
//!
//! A refusal stands for the `Retry-After` the cloud sent with it, and never
//! less than [`STANDS_FOR_AT_LEAST`]; with no `Retry-After`, for as long as
//! the refused request took to come back. Both are the route's own word on
//! how soon another request is worth sending: the first is the cloud's, and
//! the second is how long its tries at the model ran before it gave up.
//!
//! An answer the cloud gave from what it already held (`cached: true` on the
//! envelope) is no answer of the model's, and leaves a silent route silent.
//!
//! A route that comes back is used from its first answer on. What the rule
//! gives up, in the run it applies to, is two things. On a route that answers
//! some requests and refuses others after its budget is gone, calls are left
//! owed that seven attempts each might have got answered. And a call behind
//! a refusal is not sent even when the cloud already holds its answer, which
//! it would have given whatever the model was doing. Both are owed, and the
//! next scan, which starts the route's budget from nothing, sends them.
//!
//! # The bound
//!
//! Before this rule a spent budget fell back to seven attempts a call. A
//! refused route is cut to two requests at a time (`limiter::LIMIT_FLOOR`)
//! and a refused request takes about 14 s to come back, so each file cost
//! seven of those: about 49 s of the route, and as much again when the run's
//! retry of owed work asked a second time.
//!
//! With it, a route that never answers holds a scan for no more than its
//! budget, two refused requests (the one before the ledger is first charged
//! and the one that finds it spent), the last sleep of the calls waiting when
//! it ran out (64 s at most) and a probe: 46.8 minutes, whatever the number
//! of files. The retry of owed work then costs one probe.
//!
//! | Files | Seven attempts: first pass, retry pass | The probe rule: first pass, retry pass |
//! |---|---|---|
//! | 60 | 93.5 min, 45.2 min | 45.2 min, 14 s |
//! | 1,000 | 861.5 min, 812.9 min | 45.3 min, 14 s |
//!
//! Measured on a paused clock by
//! `a_route_that_never_answers_holds_a_scan_for_its_budget_whatever_the_files`
//! in `agent_service`; the first passes of the left column are from that
//! test's predecessor on the commit before this rule, and its retry passes
//! are the test's counting arm, which is what a spent budget fell back to.
//! What does grow is the probes: a service whose first file arrives after the
//! last refusal has stopped standing sends one, so a scan pays at most one
//! refused request for each such arrival, not one for each file.
//!
//! Kept beside the limiter rather than in it. The limiter decides how many
//! requests a refused route carries at once; this decides how long a call
//! keeps asking. Neither reads the other.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::retry_budget::{Charge, Ledger};

/// One call's open wait on its route's refusal. The route's ledger is charged
/// until it drops.
pub(crate) type Waiting = Charge<Arc<Ledger>>;

/// How much refused time passes between two terminal lines about it. The
/// limiter's own line says the route is slow when its limit moves, and a
/// limit at its floor does not move: without this a long window is a scan
/// that prints nothing.
const WAITING_LINE_EVERY: Duration = Duration::from_secs(5 * 60);

/// The least time a silent route's refusal stands for the calls that arrive
/// after it, when the cloud sent a `Retry-After` with it. The lambda's own
/// hint on a capacity refusal is ten seconds; a shorter or a zero hint would
/// have every arriving call take a turn as the probe.
pub(crate) const STANDS_FOR_AT_LEAST: Duration = Duration::from_secs(10);

/// How long a silent route's refusal stands for the calls that arrive after
/// it: the cloud's `Retry-After` under the policy's own ceiling on a sleep
/// and over [`STANDS_FOR_AT_LEAST`], or, when it sent none, the time the
/// refused request `took`. The module doc says why these two.
pub(crate) fn stands_for(
    retry_after: Option<Duration>,
    took: Duration,
    max_delay: Duration,
) -> Duration {
    match retry_after {
        Some(hint) => hint.min(max_delay).max(STANDS_FOR_AT_LEAST),
        None => took,
    }
}

/// What a capacity refusal comes to for the call that met it
/// ([`RouteRefusal::refused`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The route's budget has room for the wait: the call sleeps and asks
    /// again, and no attempt is spent.
    WaitedOut,
    /// Counted as every other retriable error is: it costs the call an
    /// attempt.
    CostsAnAttempt,
    /// The route is silent: the call ends on this refusal.
    EndsTheCall,
}

/// What a call about to put a request on the wire does first
/// ([`RouteRefusal::turn`]).
#[derive(Debug)]
pub(crate) enum Turn {
    /// Send it as any request is sent: the route is not silent, or the
    /// call's policy has no refusal budget.
    Send,
    /// Send it as the silent route's probe, the one request the route
    /// carries at a time. The turn is held until the guard drops.
    Probe(Probe),
    /// Send nothing. The route is silent and a request sent to it has just
    /// been refused: the call ends, owed for capacity.
    Owed,
}

/// The silent route's one request at a time. Dropped with nothing said about
/// the model (the request was cut, lost, or told to wait), it hands the turn
/// to the next call.
#[derive(Debug)]
pub(crate) struct Probe {
    route: Arc<RouteRefusal>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.route.state().probing = false;
        self.route.verdict.notify_waiters();
    }
}

/// What a route's model did last in this run, and what its calls are doing
/// about it.
#[derive(Debug, Default)]
struct State {
    /// When a refusal first found the budget spent. Set once a run: from
    /// then on no refusal on the route is waited out, however short its own
    /// wait would have been, so "spent" is one moment and not a reading that
    /// moves with each call's jitter.
    spent_at: Option<Instant>,
    /// Whether the last thing the route's model did, for any call of the
    /// run, was refuse for capacity.
    refusing: bool,
    /// Whether a probe is out.
    probing: bool,
    /// Until when the silent route's last refusal stands for the calls that
    /// arrive.
    stands_until: Option<Instant>,
    /// Refusals the route has given while silent. A call that waited on a
    /// probe reads its verdict here.
    refusals: u64,
    /// Calls ended on a refusal of their own request while the route was
    /// silent.
    ended_refused: u64,
    /// Calls ended with no request, behind another call's refusal.
    ended_unasked: u64,
}

impl State {
    fn silent(&self) -> bool {
        self.spent_at.is_some() && self.refusing
    }
}

/// One route's refused time in this run, and what its model did last.
#[derive(Debug)]
pub(crate) struct RouteRefusal {
    route: String,
    ledger: Arc<Ledger>,
    state: Mutex<State>,
    /// Woken when a probe's turn ends, however it ends, and when the model
    /// answers.
    verdict: Notify,
    /// How many [`WAITING_LINE_EVERY`] steps of refused time the terminal
    /// has heard about.
    said_steps: AtomicU64,
    /// Whether the terminal has heard that the budget is spent.
    said_spent: AtomicBool,
    /// How many [`WAITING_LINE_EVERY`] steps since the budget was spent the
    /// terminal has heard about.
    said_silent_steps: AtomicU64,
}

impl RouteRefusal {
    fn new(route: &str) -> Self {
        Self {
            route: route.to_string(),
            ledger: Arc::new(Ledger::new()),
            state: Mutex::new(State::default()),
            verdict: Notify::new(),
            said_steps: AtomicU64::new(0),
            said_spent: AtomicBool::new(false),
            said_silent_steps: AtomicU64::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// How long this run's calls on the route have waited on refusals,
    /// overlapping waits counted once.
    pub(crate) fn spent(&self) -> Duration {
        self.ledger.spent()
    }

    /// Whether a wait of `next` still fits in `budget`.
    fn fits(&self, next: Duration, budget: Duration) -> bool {
        self.spent() + next <= budget
    }

    /// When a refusal first found the route's budget spent in this run.
    #[cfg(test)]
    pub(crate) fn budget_spent_at(&self) -> Option<Instant> {
        self.state().spent_at
    }

    /// Whether a refusal has found the route's budget spent in this run.
    #[cfg(test)]
    pub(crate) fn budget_is_spent(&self) -> bool {
        self.budget_spent_at().is_some()
    }

    /// Whether the route is silent for a call whose policy allows it
    /// `budget` of waits. Never, for a policy that has none.
    pub(crate) fn is_silent(&self, budget: Duration) -> bool {
        !budget.is_zero() && self.state().silent()
    }

    /// What a call does before it puts a request on this route, under a
    /// policy that allows the route `budget` of waits (zero: the call is sent
    /// whatever the route is doing).
    ///
    /// A route that is not silent sends every request. On a silent one the
    /// call is the probe when none is out and no refusal still stands, ends
    /// owed when a refusal still stands, and otherwise waits for the probe
    /// that is out: refused, the call ends owed; answered, it is sent;
    /// neither, it takes its turn again.
    pub(crate) async fn turn(self: &Arc<Self>, budget: Duration) -> Turn {
        if budget.is_zero() {
            return Turn::Send;
        }
        // The route's refusals when this call began to wait on a probe.
        let mut waiting_since: Option<u64> = None;
        loop {
            // The path every request of a route that answers takes: one read
            // and nothing else.
            if !self.state().silent() {
                return Turn::Send;
            }
            // Registered before the state is read, so a verdict that lands
            // between the read and the wait is not lost.
            let verdict = self.verdict.notified();
            tokio::pin!(verdict);
            verdict.as_mut().enable();
            {
                let mut state = self.state();
                if !state.silent() {
                    return Turn::Send;
                }
                let refused_behind = waiting_since.is_some_and(|seen| state.refusals > seen);
                let still_stands = state
                    .stands_until
                    .is_some_and(|until| Instant::now() < until);
                if refused_behind || still_stands {
                    state.ended_unasked += 1;
                    return Turn::Owed;
                }
                if !state.probing {
                    state.probing = true;
                    return Turn::Probe(Probe {
                        route: Arc::clone(self),
                    });
                }
                waiting_since.get_or_insert(state.refusals);
            }
            verdict.await;
        }
    }

    /// What the capacity refusal the route has just given comes to for its
    /// call, when waiting it out would take `next` and the call's policy
    /// allows the route `budget` of such waits (zero for a call that counts
    /// every refusal as an attempt). `stands_for` is how long the refusal
    /// decides for the calls that arrive after it, should the route be
    /// silent ([`stands_for`]).
    ///
    /// What the route did before this refusal is read and the refusal
    /// recorded in one step, so of two calls refused together after an
    /// answer, one is counted and the other ended. The module doc states the
    /// rule and why.
    pub(crate) fn refused(
        &self,
        next: Duration,
        budget: Duration,
        stands_for: Duration,
    ) -> Refusal {
        let mut state = self.state();
        let was_refusing = std::mem::replace(&mut state.refusing, true);
        if budget.is_zero() {
            return Refusal::CostsAnAttempt;
        }
        if state.spent_at.is_none() && self.fits(next, budget) {
            return Refusal::WaitedOut;
        }
        let now = Instant::now();
        state.spent_at.get_or_insert(now);
        if !was_refusing {
            return Refusal::CostsAnAttempt;
        }
        state.refusals += 1;
        state.ended_refused += 1;
        state.stands_until = Some(now + stands_for);
        drop(state);
        self.verdict.notify_waiters();
        Refusal::EndsTheCall
    }

    /// The route's model answered a call. The silence is over: every call is
    /// sent again, and whatever the route refuses next is counted, not ended.
    pub(crate) fn answered(&self) {
        {
            let mut state = self.state();
            state.refusing = false;
            state.stands_until = None;
        }
        self.verdict.notify_waiters();
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

    /// Say, once a run, that the route's budget is spent and what follows.
    /// Nothing is said for a policy that never had one.
    pub(crate) fn say_spent(&self, budget: Duration) {
        if budget.is_zero() || self.said_spent.swap(true, Ordering::Relaxed) {
            return;
        }
        crate::progress::announce(&spent_line(&self.route, budget));
    }

    /// Say how many calls the route has ended while silent, once per
    /// [`WAITING_LINE_EVERY`] since its budget was spent. By then the route's
    /// limit sits at its floor and its ledger is charged no more, so neither
    /// of the other lines is due again, and a scan of many services would go
    /// on leaving their files pending with nothing on the terminal.
    pub(crate) fn say_silent(&self) {
        let (steps, refused, unasked) = {
            let state = self.state();
            let Some(spent_at) = state.spent_at else {
                return;
            };
            (
                spent_at.elapsed().as_secs() / WAITING_LINE_EVERY.as_secs(),
                state.ended_refused,
                state.ended_unasked,
            )
        };
        if steps == 0 || self.said_silent_steps.fetch_max(steps, Ordering::Relaxed) >= steps {
            return;
        }
        crate::progress::announce(&silent_line(&self.route, refused + unasked, unasked));
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
pub(crate) fn spent_line(route: &str, budget: Duration) -> String {
    format!(
        "model busy: {} has been refused for {} minutes of this scan; its calls are left pending \
         until one is answered",
        route.trim_start_matches('/'),
        budget.as_secs() / 60
    )
}

/// The line a run prints while a silent route goes on ending calls: how many
/// so far, and how many of those were sent no request.
pub(crate) fn silent_line(route: &str, ended: u64, unasked: u64) -> String {
    format!(
        "model busy: {} still answers nothing; {} calls left pending so far, {} of them without \
         a request",
        route.trim_start_matches('/'),
        ended,
        unasked
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

    /// Any wait a refusal would take in these tests: a few seconds, as the
    /// first backoff is.
    const NEXT: Duration = Duration::from_secs(4);

    /// How long a refusal stands in these tests.
    const STANDS: Duration = Duration::from_secs(10);

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

    /// A route whose calls have waited on refusals for `refused_for`, the
    /// last thing its model did being a refusal.
    async fn refused_for(refused_for: Duration) -> Arc<RouteRefusal> {
        let route = RouteRefusals::new().for_route("/analyze-file");
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::WaitedOut);
        let waiting = route.wait();
        tokio::time::sleep(refused_for).await;
        drop(waiting);
        route
    }

    /// A route that has spent its budget and just refused a call: silent,
    /// with that refusal no longer standing.
    async fn silent() -> Arc<RouteRefusal> {
        let route = refused_for(BUDGET).await;
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        tokio::time::sleep(STANDS).await;
        route
    }

    /// A call that asks for its turn on `route` on a task of its own.
    fn arrives(route: &Arc<RouteRefusal>) -> tokio::task::JoinHandle<Turn> {
        let route = Arc::clone(route);
        tokio::spawn(async move { route.turn(BUDGET).await })
    }

    /// Let every task that can run do so, without moving the clock.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    /// Nothing changes while the budget lasts: a refusal is waited out
    /// whatever the route did last, an answer or any number of refusals, no
    /// refusal finds the budget spent, and every request is sent.
    #[tokio::test(start_paused = true)]
    async fn while_the_budget_lasts_every_request_is_sent_and_every_refusal_waited_out() {
        let route = refused_for(BUDGET - NEXT * 2).await;
        for _ in 0..50 {
            assert!(matches!(route.turn(BUDGET).await, Turn::Send));
            assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::WaitedOut);
        }
        route.answered();
        assert!(matches!(route.turn(BUDGET).await, Turn::Send));
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::WaitedOut);
        assert!(!route.budget_is_spent());
        assert!(!route.is_silent(BUDGET));
    }

    /// carrick#1909: with the budget spent, a route whose model has answered
    /// nothing since it last refused ends the call it refuses. One answer
    /// puts the next refusal back on the count, and that refusal, unanswered,
    /// makes the route silent again.
    #[tokio::test(start_paused = true)]
    async fn a_spent_budget_ends_the_calls_a_route_refuses_until_its_model_answers() {
        let route = refused_for(BUDGET).await;
        assert!(
            !route.is_silent(BUDGET),
            "silent before its budget is spent"
        );
        for _ in 0..3 {
            assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        }
        assert!(route.budget_is_spent());
        assert!(route.is_silent(BUDGET));

        route.answered();
        assert!(!route.is_silent(BUDGET));
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::CostsAnAttempt);
        assert!(route.is_silent(BUDGET));
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);

        // A route that answers between its refusals never ends a call.
        for _ in 0..7 {
            route.answered();
            assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::CostsAnAttempt);
        }
        // Two answers running are one answer: the silence ended once.
        route.answered();
        route.answered();
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::CostsAnAttempt);
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
    }

    /// The budget is spent at one moment for every call. A wait that would
    /// cross it finds it spent, and from then on a shorter wait that would
    /// still have fitted is not taken either.
    #[tokio::test(start_paused = true)]
    async fn a_budget_found_spent_stays_spent_for_every_wait() {
        let route = refused_for(BUDGET - Duration::from_secs(30)).await;
        assert!(route.fits(NEXT, BUDGET));
        assert_eq!(
            route.refused(Duration::from_secs(60), BUDGET, STANDS),
            Refusal::EndsTheCall
        );
        assert!(route.fits(NEXT, BUDGET), "the ledger itself still has room");
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
    }

    /// A call whose policy has no refusal budget counts every refusal as an
    /// attempt, as it always did, on a route in any state. It neither spends
    /// the route's budget, nor waits for its probe, nor is ended by its
    /// silence.
    #[tokio::test(start_paused = true)]
    async fn a_call_with_no_refusal_budget_is_sent_and_counted_whatever_its_route_is_doing() {
        let fresh = RouteRefusals::new().for_route("/framework-guidance");
        for _ in 0..7 {
            assert!(matches!(fresh.turn(Duration::ZERO).await, Turn::Send));
            assert_eq!(
                fresh.refused(NEXT, Duration::ZERO, STANDS),
                Refusal::CostsAnAttempt
            );
        }
        assert!(!fresh.budget_is_spent());

        let silent = refused_for(BUDGET).await;
        assert_eq!(silent.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        // The refusal still stands, and a probe is out besides.
        assert!(matches!(silent.turn(Duration::ZERO).await, Turn::Send));
        assert!(!silent.is_silent(Duration::ZERO));
        assert_eq!(
            silent.refused(NEXT, Duration::ZERO, STANDS),
            Refusal::CostsAnAttempt
        );
    }

    /// carrick#1909, the probe. A silent route carries one request at a time.
    /// The calls that find it out wait and send nothing, and when it is
    /// refused they end owed, as does every call that arrives while the
    /// refusal stands. When it no longer stands, the next call is the probe.
    #[tokio::test(start_paused = true)]
    async fn a_refused_probe_decides_for_the_calls_behind_it_and_for_those_that_arrive() {
        let route = silent().await;
        let Turn::Probe(probe) = route.turn(BUDGET).await else {
            panic!("the first call on a silent route is its probe");
        };
        let behind: Vec<_> = (0..5).map(|_| arrives(&route)).collect();
        settle().await;
        assert!(
            behind.iter().all(|call| !call.is_finished()),
            "a call was given a turn while a probe was out"
        );
        // However long the probe takes, they wait.
        tokio::time::sleep(Duration::from_secs(14)).await;
        settle().await;
        assert!(behind.iter().all(|call| !call.is_finished()));

        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        drop(probe);
        for call in behind {
            assert!(matches!(call.await.unwrap(), Turn::Owed));
        }

        // While the refusal stands, an arriving call ends at once.
        tokio::time::sleep(STANDS - Duration::from_secs(1)).await;
        assert!(matches!(route.turn(BUDGET).await, Turn::Owed));
        // Once it does not, the next call is the probe, and one only.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Turn::Probe(_probe) = route.turn(BUDGET).await else {
            panic!("the refusal no longer stands, so the next call is the probe");
        };
        let second = arrives(&route);
        settle().await;
        assert!(!second.is_finished(), "two probes were out at once");
        second.abort();
    }

    /// A refusal decides for the calls already waiting however short a time
    /// it stands: they are read by the refusal, not by the clock.
    #[tokio::test(start_paused = true)]
    async fn a_refusal_that_stands_for_no_time_still_ends_the_calls_that_were_waiting() {
        let route = silent().await;
        let Turn::Probe(probe) = route.turn(BUDGET).await else {
            panic!("the first call on a silent route is its probe");
        };
        let behind = arrives(&route);
        settle().await;
        assert_eq!(
            route.refused(NEXT, BUDGET, Duration::ZERO),
            Refusal::EndsTheCall
        );
        drop(probe);
        assert!(matches!(behind.await.unwrap(), Turn::Owed));
        // Nothing stands, so the call after them is a probe.
        assert!(matches!(route.turn(BUDGET).await, Turn::Probe(_)));
    }

    /// One answer from the model ends the silence at once: the calls waiting
    /// on the probe are sent, and so is every call after them, though the
    /// refusal before it would still have stood.
    #[tokio::test(start_paused = true)]
    async fn an_answered_probe_sends_every_call_behind_it() {
        let route = refused_for(BUDGET).await;
        assert_eq!(
            route.refused(NEXT, BUDGET, Duration::ZERO),
            Refusal::EndsTheCall
        );
        let Turn::Probe(probe) = route.turn(BUDGET).await else {
            panic!("the first call on a silent route is its probe");
        };
        let behind: Vec<_> = (0..5).map(|_| arrives(&route)).collect();
        settle().await;

        route.answered();
        drop(probe);
        for call in behind {
            assert!(matches!(call.await.unwrap(), Turn::Send));
        }
        assert!(matches!(route.turn(BUDGET).await, Turn::Send));

        // A refusal that stood when the answer came stands no longer: the
        // route's next refusal is counted, and the call after it is a probe.
        let route = refused_for(BUDGET).await;
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        assert!(matches!(route.turn(BUDGET).await, Turn::Owed));
        route.answered();
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::CostsAnAttempt);
        assert!(matches!(route.turn(BUDGET).await, Turn::Probe(_)));
    }

    /// A probe that comes to nothing (cut, lost, told to wait) says nothing
    /// about the model. The turn passes to one of the calls behind it, and
    /// the rest go on waiting.
    #[tokio::test(start_paused = true)]
    async fn a_probe_with_no_verdict_hands_its_turn_to_one_call_behind_it() {
        let route = silent().await;
        let Turn::Probe(probe) = route.turn(BUDGET).await else {
            panic!("the first call on a silent route is its probe");
        };
        let behind: Vec<_> = (0..3).map(|_| arrives(&route)).collect();
        settle().await;
        drop(probe);
        settle().await;

        let mut probes = Vec::new();
        let mut waiting = Vec::new();
        for call in behind {
            if call.is_finished() {
                probes.push(call.await.unwrap());
            } else {
                waiting.push(call);
            }
        }
        assert_eq!(probes.len(), 1, "one call takes the turn");
        assert!(matches!(probes[0], Turn::Probe(_)));
        assert_eq!(waiting.len(), 2);

        // The new probe is refused, and that decides for the two.
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        drop(probes);
        for call in waiting {
            assert!(matches!(call.await.unwrap(), Turn::Owed));
        }
    }

    /// How long a refusal stands: the cloud's hint, never under ten seconds
    /// and never over the policy's ceiling on one sleep, or the time the
    /// refused request took when the cloud sent no hint.
    #[test]
    fn a_refusal_stands_for_the_clouds_hint_or_for_the_time_it_took() {
        let secs = Duration::from_secs;
        let (took, ceiling) = (secs(14), secs(64));
        assert_eq!(stands_for(Some(secs(10)), took, ceiling), secs(10));
        assert_eq!(stands_for(Some(secs(30)), took, ceiling), secs(30));
        assert_eq!(stands_for(Some(Duration::ZERO), took, ceiling), secs(10));
        assert_eq!(stands_for(Some(secs(120)), took, ceiling), ceiling);
        assert_eq!(stands_for(None, took, ceiling), took);
        assert_eq!(stands_for(None, secs(90), ceiling), secs(90));
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
        let spent = spent_line("/analyze-file", budget);
        assert_eq!(
            spent,
            "model busy: analyze-file has been refused for 45 minutes of this scan; its calls \
             are left pending until one is answered"
        );
        let silent = silent_line("/analyze-file", 86, 80);
        assert_eq!(
            silent,
            "model busy: analyze-file still answers nothing; 86 calls left pending so far, 80 of \
             them without a request"
        );
        for line in [waiting, spent, silent] {
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

    /// A silent route still prints: one line for every five minutes since
    /// its budget was spent, counting the calls it has ended and how many of
    /// them were sent nothing. Nothing is due before the budget is spent.
    #[tokio::test(start_paused = true)]
    async fn the_silent_line_is_due_once_per_five_minutes_since_the_budget_was_spent() {
        let route = refused_for(BUDGET).await;
        let due = |route: &RouteRefusal| {
            let before = route.said_silent_steps.load(Ordering::Relaxed);
            route.say_silent();
            route.said_silent_steps.load(Ordering::Relaxed) > before
        };
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(!due(&route), "said before the budget was found spent");

        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        assert!(matches!(route.turn(BUDGET).await, Turn::Owed));
        tokio::time::sleep(Duration::from_secs(299)).await;
        assert!(!due(&route));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(route.refused(NEXT, BUDGET, STANDS), Refusal::EndsTheCall);
        assert!(due(&route));
        assert!(!due(&route), "said twice for one step");
        let state = route.state();
        assert_eq!((state.ended_refused, state.ended_unasked), (2, 1));
    }
}
