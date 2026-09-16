//! Adaptive concurrency for model requests, one limit per cloud route, and an
//! adaptive request rate across all of them (carrick-cloud#869).
//!
//! Two signals, two mechanisms. A model that is out of capacity answers 503
//! `model_error` with `Retry-After`, per model, so it cuts its route's
//! concurrency ([`AdaptiveLimit`]). The API Gateway in front of every route
//! answers 429 with no envelope when the scan sends too many requests a
//! second, whichever route they are for, so it cuts the shared rate
//! ([`RatePacer`]).
//!
//! The process-wide semaphore in `agent_service` bounds requests on the wire,
//! but a fixed bound cannot tell a healthy model from a busy one. Under a
//! capacity dip every refused request frees its slot at once, the next queued
//! request takes it and is refused too, and the stage burns its retry budgets
//! at the full width of the cap. On 2026-09-15 the detection model was busy
//! while the intent model was not, so the limit is kept per route: one busy
//! model slows its own requests and leaves the others alone.
//!
//! Each route's limit follows AIMD, the rule TCP uses for the same problem:
//!
//! - **Multiplicative decrease.** A request the model refused for capacity
//!   (a 503 envelope, or any envelope carrying `Retry-After`) halves the
//!   limit, down to a floor. Only once per epoch: the requests already in
//!   flight when the limit was cut were admitted under the old width, so
//!   their refusals are one signal, not twenty. The file-analyzer's lease
//!   wait (`analysis_in_flight`) is not a refusal whatever its status and
//!   headers: the caller releases its slot with no verdict (carrick#1131).
//! - **Additive increase.** After as many successes in a row as the current
//!   limit (one round at that width), the limit rises by one, back towards the
//!   configured maximum.
//! - **Restore after a quiet spell.** A route that has gone
//!   [`QUIET_RESTORE`] without a refusal goes back to its maximum in one
//!   step. AIMD's climb assumes the backend's capacity is a level this scan
//!   is probing for; a model on dynamic shared quota refuses by the window a
//!   request lands in, and when the window passes there is nothing left to
//!   probe for. Without this the scan keeps paying for a dip that is over.
//!
//! The maximum is `CARRICK_CONCURRENCY_LIMIT`, so the env knob still caps
//! every route. A retry waiting out its backoff holds no slot here either
//! (carrick#1077): the caller reports the refusal, the slot is released, and
//! the retry queues again when it wakes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tracing::debug;

/// The lowest a route's limit falls. One would serialise a whole stage behind
/// its slowest request; two keeps a stage moving while putting almost nothing
/// on a busy model. A configured maximum below it wins.
pub(crate) const LIMIT_FLOOR: usize = 2;

/// How often the terminal hears that a route's limit changed while it stays
/// throttled. The file log gets every change; a person watching a scan needs
/// a line, not a line per refused request.
const TERMINAL_LINE_GAP: Duration = Duration::from_secs(15);

/// How long a route must go without a capacity refusal before its limit
/// returns to the maximum in one step.
///
/// The decrease answers a busy model; this answers the model going quiet
/// again. A refusal on dynamic shared quota is a property of the window the
/// request landed in, not of how fast this scan is sending (carrick-cloud#869,
/// measured on the 2026-09-14 first index: refusals ran at 13.4% of model
/// calls inside two dips of about twenty minutes and 0.3% between them, while
/// the scan's own model-call rate varied by less than a third across both).
/// A scan that keeps climbing back one slot at a time after the window has
/// passed pays for a dip long after it ended: from 14 back to 28 is 294
/// consecutive successes, and on a route whose calls are slow that is minutes
/// (2026-09-15 scale test: file analysis sat between 3 and 8 for three
/// minutes and had reached only 23 of 28 when the run ended).
///
/// What keeps this safe on a backend that really does have a fixed number of
/// slots — where the refusals ARE this scan's own doing — is the additive
/// increase underneath it: the climb re-probes, meets the backend's edge, and
/// that refusal restarts the spell, so the spell never elapses while the
/// backend is still refusing. A spell has to be long enough for that probe to
/// get there. Against a fixed-capacity stub, a spell of four call latencies
/// is too short and costs 585 refusals and 18 calls lost against the old
/// rule's 49 and none; at nine and at twenty it is 49 and none, the old
/// rule's numbers exactly, whether the backend's edge sits below or above
/// half the maximum.
///
/// A minute, then: several times the climb back on the slowest route
/// (file analysis, 3.4 s p50), and nothing against the tens of minutes of
/// scan that follow a dip. It is also far longer than the gap between two
/// refusals inside a dip — at the floor of two and the 15% per-call refusal
/// rate of 2026-09-14, about 11 s on that route and 5 s on the intent route
/// — so a dip cannot restore into itself.
///
/// The rule rejected: restoring once the climb passes a fraction of the
/// maximum, which needs no clock. On a fixed-capacity backend whose edge sits
/// in the top half it oscillates — 188 refusals against the old rule's 12.
///
/// The measurements are `probe_869_capacity_shape` and `probe_869_dsq_shape*`
/// in `agent_service`'s tests, `#[ignore]`d because they print a table rather
/// than assert one.
const QUIET_RESTORE: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct State {
    limit: usize,
    in_flight: usize,
    /// Bumped on every decrease. A refusal from a request admitted in an
    /// earlier epoch was already answered by that decrease.
    epoch: u64,
    /// Successes since the last change to `limit`.
    run: usize,
    /// When this route was last refused for capacity. `None` until the first
    /// refusal, and again once a quiet spell has restored the limit.
    last_refusal: Option<Instant>,
    /// When the terminal last heard this route was busy, and at what limit.
    /// `None` while the route runs at its maximum.
    announced: Option<(Instant, usize)>,
}

/// One route's adaptive limit.
#[derive(Debug)]
pub(crate) struct AdaptiveLimit {
    route: String,
    max: usize,
    floor: usize,
    quiet: Duration,
    state: Mutex<State>,
    notify: Notify,
}

impl AdaptiveLimit {
    /// One route's limit, with the quiet spell that restores it passed in:
    /// [`RouteLimits`] passes [`QUIET_RESTORE`], and a test passes a spell it
    /// does not have to wait a minute for.
    pub(crate) fn with_quiet(route: &str, max: usize, quiet: Duration) -> Self {
        let max = max.max(1);
        Self {
            route: route.to_string(),
            max,
            floor: LIMIT_FLOOR.min(max),
            quiet,
            state: Mutex::new(State {
                limit: max,
                in_flight: 0,
                epoch: 0,
                run: 0,
                last_refusal: None,
                announced: None,
            }),
            notify: Notify::new(),
        }
    }

    /// The current limit.
    #[cfg(test)]
    pub(crate) fn limit(&self) -> usize {
        self.state.lock().unwrap().limit
    }

    /// Wait for a slot under the current limit.
    ///
    /// The waiter registers for a wake-up BEFORE it reads the count, so a
    /// release that lands between the read and the wait is not lost.
    pub(crate) async fn acquire(self: &Arc<Self>) -> Slot {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap();
                if state.in_flight < state.limit {
                    state.in_flight += 1;
                    return Slot {
                        limit: Arc::clone(self),
                        epoch: state.epoch,
                    };
                }
            }
            notified.await;
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().in_flight -= 1;
        // One slot came free, so one waiter. `Notify` hands the wake-up on if
        // that waiter is dropped before it runs.
        self.notify.notify_one();
    }

    fn succeeded(&self) {
        let grown = {
            let mut state = self.state.lock().unwrap();
            if state.limit >= self.max {
                None
            } else if state
                .last_refusal
                .is_some_and(|at| Instant::now().duration_since(at) >= self.quiet)
            {
                // Nothing has been refused for a whole quiet spell, so the
                // window that refused this route has passed. Climbing back a
                // slot at a time from here would go on paying for it.
                state.limit = self.max;
                state.run = 0;
                state.last_refusal = None;
                state.announced = None;
                Some(state.limit)
            } else {
                state.run += 1;
                if state.run >= state.limit {
                    state.limit += 1;
                    state.run = 0;
                    if state.limit == self.max {
                        state.announced = None;
                    }
                    Some(state.limit)
                } else {
                    None
                }
            }
        };
        let Some(limit) = grown else {
            return;
        };
        debug!(
            route = %self.route,
            limit,
            max = self.max,
            "model requests: limit raised"
        );
        if limit == self.max {
            crate::progress::announce(&format!(
                "model capacity back: {} at {} requests at a time",
                route_label(&self.route),
                limit
            ));
        }
        // The limit rose on top of whatever release follows, so more than one
        // waiter may now fit; the ones that do not go back to waiting.
        self.notify.notify_waiters();
    }

    fn overloaded(&self, admitted_in: u64) {
        let (before, after, in_flight, announce) = {
            let mut state = self.state.lock().unwrap();
            // Every refusal restarts the quiet spell, even one this epoch has
            // already answered: the spell measures how long the model has
            // gone without refusing anything, not how long since the last cut.
            state.last_refusal = Some(Instant::now());
            if admitted_in != state.epoch {
                return;
            }
            let before = state.limit;
            state.limit = (state.limit / 2).max(self.floor);
            state.epoch += 1;
            state.run = 0;
            let after = state.limit;
            // The first cut is announced; after that, a changed limit at most
            // once per gap.
            let now = Instant::now();
            let due = match state.announced {
                None => true,
                Some((at, said)) => after != said && now.duration_since(at) >= TERMINAL_LINE_GAP,
            };
            if due {
                state.announced = Some((now, after));
            }
            (before, after, state.in_flight, due)
        };
        debug!(
            route = %self.route,
            before,
            after,
            in_flight,
            "model requests: limit cut after a capacity refusal"
        );
        if announce {
            // Announced rather than only logged, so a scan the indexer runs
            // as a child still reaches the person watching it (carrick#1122).
            crate::progress::announce(&format!(
                "model busy: slowing {} to {} requests at a time",
                route_label(&self.route),
                after
            ));
        }
    }
}

/// `/analyze-file` reads as `analyze-file` in a sentence.
fn route_label(route: &str) -> &str {
    route.trim_start_matches('/')
}

/// A slot held for one HTTP attempt. Dropping it releases the slot with no
/// verdict, which is what a transport failure or a permanent error is: neither
/// says anything about the model's capacity.
#[derive(Debug)]
pub(crate) struct Slot {
    limit: Arc<AdaptiveLimit>,
    epoch: u64,
}

impl Slot {
    /// The model answered. Counts towards raising the limit.
    pub(crate) fn succeeded(self) {
        self.limit.succeeded();
    }

    /// The cloud refused this request for capacity. Cuts the limit (once per
    /// epoch) and releases the slot, so the caller's retry sleeps holding
    /// nothing.
    pub(crate) fn overloaded(self) {
        self.limit.overloaded(self.epoch);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.limit.release();
    }
}

/// Every route's limit, created on first use, each starting at the maximum.
#[derive(Debug)]
pub(crate) struct RouteLimits {
    max: usize,
    quiet: Duration,
    routes: Mutex<BTreeMap<String, Arc<AdaptiveLimit>>>,
}

impl RouteLimits {
    pub(crate) fn new(max: usize) -> Self {
        Self::with_quiet(max, QUIET_RESTORE)
    }

    /// [`Self::new`] with the quiet spell that restores a route's limit set
    /// explicitly. A test on its own timescale passes its own spell, and
    /// `Duration::MAX` leaves the additive increase as the only way back.
    pub(crate) fn with_quiet(max: usize, quiet: Duration) -> Self {
        Self {
            max,
            quiet,
            routes: Mutex::new(BTreeMap::new()),
        }
    }

    /// The limit for `route`. Keyed on the route rather than on a list of
    /// models: which model serves a route is the cloud's business, and the
    /// route is the finest grain the scanner can see.
    pub(crate) fn for_route(&self, route: &str) -> Arc<AdaptiveLimit> {
        let mut routes = self.routes.lock().unwrap();
        Arc::clone(
            routes.entry(route.to_string()).or_insert_with(|| {
                Arc::new(AdaptiveLimit::with_quiet(route, self.max, self.quiet))
            }),
        )
    }
}

/// The lowest request rate the pacer falls to, per second.
pub(crate) const RATE_FLOOR: u32 = 2;

/// The window the pacer reads the rate it was sending at from, when a gateway
/// throttle is the first sign it was too fast.
const RATE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct PaceState {
    /// Requests per second, or `None` while nothing has throttled this run.
    rate: Option<u32>,
    /// The earliest the next request may go.
    next_at: tokio::time::Instant,
    /// Bumped on every cut, for the same reason as [`State::epoch`].
    epoch: u64,
    /// Requests admitted since the last change to `rate`.
    run: u32,
    /// Send times within the last [`RATE_WINDOW`].
    recent: std::collections::VecDeque<tokio::time::Instant>,
    announced: Option<(tokio::time::Instant, u32)>,
}

/// Requests per second across every route, adapting to the gateway throttle.
///
/// Concurrency does not bound rate. A cached answer comes back in tens of
/// milliseconds, so a few dozen slots can put hundreds of requests a second on
/// the wire, and the API Gateway throttle in front of every route is a rate,
/// shared by all of them. On 2026-09-14 its 429s were tripped by cache-hit
/// intent replies, not by slow model calls, and cutting a route's concurrency
/// would not have slowed them.
///
/// So a gateway 429 is a separate signal with its own AIMD. The pacer starts
/// with no rate at all. The first throttle sets one at half the rate it was
/// actually sending at; each later throttle halves it again, once per epoch,
/// down to [`RATE_FLOOR`]; every request the gateway lets through counts
/// towards raising it by one a second. No ceiling is configured: the gateway's
/// is the cloud's to set, and the first throttle says where it is.
#[derive(Debug)]
pub(crate) struct RatePacer {
    state: Mutex<PaceState>,
}

/// When a request may go, and under which epoch it was paced.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reservation {
    pub(crate) at: tokio::time::Instant,
    pub(crate) epoch: u64,
}

impl RatePacer {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PaceState {
                rate: None,
                next_at: tokio::time::Instant::now(),
                epoch: 0,
                run: 0,
                recent: std::collections::VecDeque::new(),
                announced: None,
            }),
        }
    }

    /// The current rate, `None` while unpaced.
    #[cfg(test)]
    pub(crate) fn rate(&self) -> Option<u32> {
        self.state.lock().unwrap().rate
    }

    /// Reserve the next send time. The caller sleeps until `at` and then
    /// sends; the reservation is taken once the attempt holds its slots, so
    /// the time it names is the time the request goes.
    pub(crate) fn reserve(&self) -> Reservation {
        let now = tokio::time::Instant::now();
        let mut state = self.state.lock().unwrap();
        let at = state.next_at.max(now);
        state.next_at = match state.rate {
            Some(rate) => at + Duration::from_secs(1) / rate,
            None => at,
        };
        while state
            .recent
            .front()
            .is_some_and(|sent| now.saturating_duration_since(*sent) > RATE_WINDOW)
        {
            state.recent.pop_front();
        }
        state.recent.push_back(at);
        Reservation {
            at,
            epoch: state.epoch,
        }
    }

    /// The gateway let a request through.
    pub(crate) fn admitted(&self) {
        let mut state = self.state.lock().unwrap();
        let Some(rate) = state.rate else {
            return;
        };
        state.run += 1;
        if state.run >= rate {
            state.rate = Some(rate + 1);
            state.run = 0;
            debug!(
                rate = rate + 1,
                "model requests: pace raised after a run the gateway let through"
            );
        }
    }

    /// The gateway throttled a request paced under `epoch`.
    pub(crate) fn throttled(&self, epoch: u64) {
        let (before, after, announce) = {
            let mut state = self.state.lock().unwrap();
            if epoch != state.epoch {
                return;
            }
            let sending = state
                .rate
                .unwrap_or_else(|| u32::try_from(state.recent.len()).unwrap_or(u32::MAX));
            let after = (sending / 2).max(RATE_FLOOR);
            let before = state.rate;
            state.rate = Some(after);
            state.epoch += 1;
            state.run = 0;
            let now = tokio::time::Instant::now();
            // The new pace starts one interval out. Starting it now would send
            // the next request straight into the bucket the throttle just
            // said was empty, and its 429 would cut the pace a second time for
            // the same burst.
            state.next_at = state.next_at.max(now + Duration::from_secs(1) / after);
            let due = match state.announced {
                None => true,
                Some((at, said)) => {
                    after != said && now.saturating_duration_since(at) >= TERMINAL_LINE_GAP
                }
            };
            if due {
                state.announced = Some((now, after));
            }
            (before, after, due)
        };
        debug!(
            before = ?before,
            after,
            "model requests: pace cut after a gateway throttle"
        );
        if announce {
            crate::progress::announce(&format!(
                "gateway busy: pacing model requests to {after} a second"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn take(limit: &Arc<AdaptiveLimit>, n: usize) -> Vec<Slot> {
        let mut slots = Vec::with_capacity(n);
        for _ in 0..n {
            slots.push(limit.acquire().await);
        }
        slots
    }

    /// A limit carrying the quiet spell the scanner ships, which no test here
    /// stays still long enough to reach.
    fn limit_of(route: &str, max: usize) -> Arc<AdaptiveLimit> {
        Arc::new(AdaptiveLimit::with_quiet(route, max, QUIET_RESTORE))
    }

    #[tokio::test]
    async fn a_refusal_halves_the_limit_once_per_epoch_down_to_the_floor() {
        let limit = limit_of("/analyze-file", 16);
        let slots = take(&limit, 16).await;
        assert_eq!(limit.limit(), 16);

        // Sixteen requests admitted together, all refused: one signal.
        for slot in slots {
            slot.overloaded();
        }
        assert_eq!(limit.limit(), 8);

        // A request admitted after the cut is a new signal.
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 4);
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), LIMIT_FLOOR);
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), LIMIT_FLOOR, "fell through the floor");
    }

    #[tokio::test]
    async fn a_run_of_successes_as_long_as_the_limit_raises_it_by_one_up_to_the_max() {
        let limit = limit_of("/generate-intent", 4);
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 2);

        limit.acquire().await.succeeded();
        assert_eq!(limit.limit(), 2);
        limit.acquire().await.succeeded();
        assert_eq!(limit.limit(), 3);
        for _ in 0..3 {
            limit.acquire().await.succeeded();
        }
        assert_eq!(limit.limit(), 4);
        for _ in 0..20 {
            limit.acquire().await.succeeded();
        }
        assert_eq!(limit.limit(), 4, "grew past the configured maximum");
    }

    /// carrick-cloud#869: once a route has gone a whole quiet spell without a
    /// refusal, the next success puts it straight back at its maximum, rather
    /// than buying the 2 + 3 + ... + 27 successes the climb from the floor to
    /// 28 would otherwise cost.
    #[tokio::test]
    async fn a_quiet_spell_without_a_refusal_restores_the_limit_in_one_step() {
        let limit = Arc::new(AdaptiveLimit::with_quiet(
            "/generate-intent",
            16,
            Duration::from_millis(30),
        ));
        limit.acquire().await.overloaded();
        limit.acquire().await.overloaded();
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), LIMIT_FLOOR);

        // Inside the spell, only the additive increase is on offer.
        limit.acquire().await.succeeded();
        limit.acquire().await.succeeded();
        assert_eq!(limit.limit(), 3);

        tokio::time::sleep(Duration::from_millis(40)).await;
        limit.acquire().await.succeeded();
        assert_eq!(
            limit.limit(),
            16,
            "the quiet spell did not restore the limit"
        );

        // Restoring clears the spell, so a fresh refusal still cuts.
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 8);
    }

    /// A backend that keeps refusing keeps the limit down: every refusal
    /// restarts the quiet spell, including one this epoch has already
    /// answered, so a spell only elapses when the refusals really have
    /// stopped.
    #[tokio::test]
    async fn a_refusal_inside_the_quiet_spell_restarts_it() {
        let limit = Arc::new(AdaptiveLimit::with_quiet(
            "/analyze-file",
            8,
            Duration::from_millis(60),
        ));
        // Taken before the cut, so each one's epoch is stale afterwards: a
        // refusal the limit has already answered, which must still count as
        // the model having refused something.
        let mut stale = Vec::new();
        for _ in 0..3 {
            stale.push(limit.acquire().await);
        }
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 4);

        for slot in stale {
            tokio::time::sleep(Duration::from_millis(45)).await;
            slot.overloaded();
            limit.acquire().await.succeeded();
            assert_eq!(
                limit.limit(),
                4,
                "restored while refusals were still coming"
            );
        }

        tokio::time::sleep(Duration::from_millis(70)).await;
        limit.acquire().await.succeeded();
        assert_eq!(limit.limit(), 8);
    }

    #[tokio::test]
    async fn a_cut_restarts_the_run_of_successes() {
        let limit = limit_of("/analyze-file", 8);
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 4);
        for _ in 0..3 {
            limit.acquire().await.succeeded();
        }
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 2);
        // Three successes before the cut would reach a run of four here, and
        // raise the limit on the first success after it.
        limit.acquire().await.succeeded();
        assert_eq!(
            limit.limit(),
            2,
            "successes from before the cut carried over"
        );
        limit.acquire().await.succeeded();
        assert_eq!(limit.limit(), 3);
    }

    #[tokio::test]
    async fn a_request_waits_while_in_flight_is_over_a_cut_limit() {
        let limit = limit_of("/analyze-file", 8);
        let mut held = take(&limit, 8).await;
        // Cut to 4 with 7 still in flight.
        held.pop().unwrap().overloaded();
        assert_eq!(limit.limit(), 4);

        let blocked = tokio::time::timeout(Duration::from_millis(100), limit.acquire()).await;
        assert!(blocked.is_err(), "admitted above the cut limit");

        // Releases bring in-flight to 3; the next request fits.
        held.truncate(3);
        let admitted = tokio::time::timeout(Duration::from_millis(100), limit.acquire()).await;
        assert!(
            admitted.is_ok(),
            "a free slot under the limit was not handed out"
        );
    }

    /// A release frees one slot and wakes one waiter. Growth frees a second
    /// slot on top of that release, so it must wake more than one: with only
    /// the release's wake-up, a slot under the raised limit sits idle until
    /// some other request finishes.
    #[tokio::test]
    async fn growth_wakes_every_waiter_that_now_fits() {
        let limit = limit_of("/analyze-file", 3);
        limit.acquire().await.overloaded();
        assert_eq!(limit.limit(), 2);
        let first = limit.acquire().await;
        let second = limit.acquire().await;

        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let limit = Arc::clone(&limit);
                tokio::spawn(async move { limit.acquire().await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(waiters.iter().all(|w| !w.is_finished()));

        // A run of one at limit 2: a slot is released, one waiter takes it.
        first.succeeded();
        // A run of two: the limit rises to 3 AND a slot is released, so the
        // other two waiters both fit.
        second.succeeded();
        assert_eq!(limit.limit(), 3);

        let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
        let mut admitted = Vec::new();
        for waiter in waiters {
            if let Ok(Ok(slot)) = tokio::time::timeout_at(deadline, waiter).await {
                admitted.push(slot);
            }
        }
        assert_eq!(admitted.len(), 3, "a slot under the raised limit sat idle");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unpaced_run_sends_at_once_and_the_first_throttle_halves_what_it_was_sending() {
        let pacer = RatePacer::new();
        let start = tokio::time::Instant::now();
        let mut last = None;
        for _ in 0..40 {
            let reservation = pacer.reserve();
            assert_eq!(reservation.at, start, "an unpaced request waited");
            last = Some(reservation);
        }
        assert_eq!(pacer.rate(), None);

        // Forty requests in the last second: the first throttle sets twenty.
        pacer.throttled(last.unwrap().epoch);
        assert_eq!(pacer.rate(), Some(20));
        // Throttles on requests paced before the cut are the same signal.
        pacer.throttled(last.unwrap().epoch);
        assert_eq!(pacer.rate(), Some(20));

        // Paced now: twenty a second is one every 50 ms, starting one interval
        // after the cut rather than straight back into the empty bucket.
        let first = pacer.reserve();
        assert_eq!(first.at, start + Duration::from_millis(50));
        let second = pacer.reserve();
        assert_eq!(second.at - first.at, Duration::from_millis(50));

        pacer.throttled(second.epoch);
        assert_eq!(pacer.rate(), Some(10));
    }

    #[tokio::test(start_paused = true)]
    async fn the_pace_falls_to_its_floor_and_climbs_back_one_a_second_per_run() {
        let pacer = RatePacer::new();
        for _ in 0..3 {
            let reservation = pacer.reserve();
            pacer.throttled(reservation.epoch);
        }
        assert_eq!(pacer.rate(), Some(RATE_FLOOR));

        // A run as long as the rate raises it by one.
        pacer.admitted();
        assert_eq!(pacer.rate(), Some(RATE_FLOOR));
        pacer.admitted();
        assert_eq!(pacer.rate(), Some(RATE_FLOOR + 1));
        for _ in 0..RATE_FLOOR + 1 {
            pacer.admitted();
        }
        assert_eq!(pacer.rate(), Some(RATE_FLOOR + 2));
    }

    #[tokio::test(start_paused = true)]
    async fn admissions_do_not_start_pacing_a_run_that_was_never_throttled() {
        let pacer = RatePacer::new();
        for _ in 0..100 {
            pacer.reserve();
            pacer.admitted();
        }
        assert_eq!(pacer.rate(), None);
    }

    #[test]
    fn a_maximum_below_the_floor_is_its_own_floor() {
        let limit = limit_of("/framework-detect", 1);
        assert_eq!(limit.floor, 1);
        assert_eq!(limit.limit(), 1);
    }

    #[test]
    fn each_route_has_its_own_limit() {
        let limits = RouteLimits::new(8);
        let detect = limits.for_route("/framework-detect");
        assert!(Arc::ptr_eq(&detect, &limits.for_route("/framework-detect")));
        assert!(!Arc::ptr_eq(&detect, &limits.for_route("/generate-intent")));
    }
}
