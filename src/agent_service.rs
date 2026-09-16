use crate::oidc::OidcProvider;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::time::{Duration, sleep};
use tracing::{debug, warn};

mod limiter;
use limiter::{RatePacer, RouteLimits};

/// Process-global circuit breaker for backend LLM quota exhaustion.
///
/// A scan runs as a single process, and every lambda call it makes draws on
/// the same backend LLM quota. A quota / rate-limit error therefore does not
/// clear within a scan, and — because the cloud counts each attempt before
/// it calls the model — every retry only burns more of the exhausted budget.
///
/// The dominant failure mode without this breaker: ~20 concurrent workers
/// each independently walk the full 2→64s backoff chain against a wall that
/// will never lift, so a scan can sit dead for 20+ minutes making no progress
/// while still consuming quota. The breaker collapses that: the first worker
/// to see a quota error trips it, and every other in-flight or queued call —
/// across all phases and all `AgentService` instances — aborts immediately.
///
/// This is deliberately a process-global (like [`crate::oidc::OidcProvider`]'s
/// `global()`): there are several independently-constructed `AgentService`
/// instances across a single scan, and a quota wall hit by any of them means
/// the backend is exhausted for all of them.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// Whether the quota circuit breaker has tripped this process. Public so the
/// engine can abort before uploading a quota-degraded (partial) index.
pub fn rate_limit_tripped() -> bool {
    RATE_LIMITED.load(Ordering::Relaxed)
}

/// Trip the quota circuit breaker. Idempotent.
fn trip_rate_limit() {
    RATE_LIMITED.store(true, Ordering::Relaxed);
}

/// Close the breaker again, for the one moment a scan deliberately gives the
/// backend another chance: the engine's single retry of the work a run still
/// owes, which starts only after it has waited (see `engine::durability`). A
/// quota that has not refilled trips it again on the first call, and every
/// other call fails fast as before.
pub fn reset_rate_limit() {
    RATE_LIMITED.store(false, Ordering::Relaxed);
}

/// Calls this process failed fast because the breaker was open, or that
/// tripped it.
///
/// A file or an intent that ends this way is not counted as lost (it was
/// never attempted), so the count is what tells the engine that a service
/// analysed while the breaker was open has model work it did not do.
static QUOTA_ABORTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many calls have failed on the quota breaker so far this process.
pub fn quota_abort_count() -> usize {
    QUOTA_ABORTS.load(Ordering::Relaxed)
}

/// How many HTTP attempts this process has retried, across every call.
static RETRIES: AtomicU64 = AtomicU64::new(0);

/// When the terminal was last told about them.
static RETRIES_ANNOUNCED: Mutex<Option<Instant>> = Mutex::new(None);

/// How long the terminal's retry line stays quiet after it has spoken.
const RETRY_ANNOUNCE_GAP: Duration = Duration::from_secs(30);

/// Count one retry, and say so on the terminal when it is due.
///
/// Each attempt's own line goes to the file log under
/// [`crate::logging::RETRY_TARGET`], which the terminal does not show: a scan
/// printed dozens of them, in transport terms a user cannot act on
/// (carrick#1103). The terminal gets this one line instead, on the first retry
/// and then at most every [`RETRY_ANNOUNCE_GAP`], with the running count.
///
/// Counted only for the retries the limiter does not already announce: a
/// model-busy refusal or a gateway throttle is stated once by the limiter's
/// own line (carrick#1119), so this covers a failed response read, a gateway
/// 5xx, a network error and a retriable error that is not the model being
/// busy.
fn note_retry() {
    let count = RETRIES.fetch_add(1, Ordering::Relaxed) + 1;
    let now = Instant::now();
    let Ok(mut last) = RETRIES_ANNOUNCED.lock() else {
        return;
    };
    if retry_announcement_due(*last, now) {
        *last = Some(now);
        crate::progress::announce(&retry_line(count));
    }
}

/// The aggregated retry line.
pub(crate) fn retry_line(count: u64) -> String {
    format!("Carrick Cloud did not answer, retrying ({count} so far)")
}

/// Whether the terminal's retry line is due: the first time, then once the
/// gap has passed.
fn retry_announcement_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= RETRY_ANNOUNCE_GAP)
}

/// How many requests this scan has issued to each cloud route.
///
/// Process-global for the same reason the breaker above is: a scan builds
/// several `AgentService` instances and the count that matters is the one the
/// whole run issued. Counted at the entry point, BEFORE the mock short-circuit,
/// so an offline run reports what a real scan would send rather than what it
/// actually sent — which is the only way to state request volume without
/// paying for it (carrick#767).
///
/// A cached answer replayed by the scanner never reaches here, so a route
/// counted here is a round trip the run really made: the number is the answer
/// to "the boundary says nothing was sent, so where did those invocations come
/// from". One per CALL, not per HTTP attempt — a call the retry loop repeats
/// is one row here and more than one invocation in the cloud.
static REQUEST_COUNTS: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();

fn request_counts_map() -> &'static Mutex<BTreeMap<String, usize>> {
    REQUEST_COUNTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn record_request(path: &str) {
    let mut counts = request_counts_map().lock().unwrap();
    *counts.entry(path.to_string()).or_insert(0) += 1;
}

/// Requests issued per route so far this scan.
pub fn request_counts() -> BTreeMap<String, usize> {
    request_counts_map().lock().unwrap().clone()
}

/// The requests issued between two snapshots, as the per-service line prints
/// them. Routes with no request in the window are omitted; a window with none
/// at all reads `none`, which is a statement and not an empty string.
pub fn requests_between(
    before: &BTreeMap<String, usize>,
    after: &BTreeMap<String, usize>,
) -> String {
    let parts: Vec<String> = after
        .iter()
        .filter_map(|(route, count)| {
            let delta = count - before.get(route).copied().unwrap_or(0);
            (delta > 0).then(|| format!("{} {}", route.trim_start_matches('/'), delta))
        })
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// Whether a cloud error envelope signals backend quota / rate-limit
/// exhaustion (which backoff cannot clear within a scan), as opposed to a
/// transient overload (which it can). The cloud maps both its own per-user
/// daily cap and upstream provider quota errors to the `rate_limited` code.
fn is_quota_error(err: &AgentError) -> bool {
    err.code == "rate_limited"
}

/// Pseudo-code for the call-level failure raised once the quota breaker is
/// open. Not a cloud code: the cloud never sends it, the scanner synthesises it
/// so callers can tell "the backend is out of quota, everything downstream is
/// doomed" apart from a genuine per-call failure.
pub const QUOTA_ABORT_CODE: &str = "quota_exhausted";

/// The cloud's code for "the model was not asked, on purpose": an operator
/// kill switch, the daily bucket on an OIDC caller, or a spend allowance.
/// `details.reason` says which, and nothing here reads it — every reason means
/// the same thing to a scan.
///
/// Distinct from [`QUOTA_ABORT_CODE`] and from `rate_limited`: this call
/// failed because a budget refused it, not because the backend is exhausted,
/// so the breaker must not trip and the run must not fail (carrick#555, and
/// C1 of carrick-cloud `docs/internal/reference/laptop-scan-seam.md`).
pub const LLM_DISABLED_CODE: &str = "llm_disabled";

/// The cloud's answer to a re-sent file whose first request is still running
/// (carrick#1131): an earlier request for the same prompt holds the
/// file-analyzer's lease, its answer had not landed when this caller's gateway
/// budget ran out, and the next request collects it from the cache. The lease
/// exists because the API Gateway cuts `/analyze-file` at 30 s while a large
/// file's model call runs longer (carrick-cloud#874).
///
/// It is a wait, not a refusal. The model was not asked by this request and
/// said nothing about its capacity, so the reply holds no slot, cuts no limit
/// and costs no attempt (see [`MAX_IN_FLIGHT_WAITS`]). The cloud sends it as
/// `409` with this code; a cloud deployed before that sent `503 model_error`
/// with `details.reason` set to the same string, and both are read the same
/// way ([`is_analysis_in_flight`]).
pub const ANALYSIS_IN_FLIGHT_CODE: &str = "analysis_in_flight";

/// Whether an error envelope is the lease wait ([`ANALYSIS_IN_FLIGHT_CODE`]),
/// in either wire shape: the code itself, or the reason on a `model_error`.
fn is_analysis_in_flight(err: &AgentError) -> bool {
    err.code == ANALYSIS_IN_FLIGHT_CODE || err.reason() == Some(ANALYSIS_IN_FLIGHT_CODE)
}

/// The error returned for an individual call once the breaker is open. Scoped
/// to what's true at the call level (this call fails fast); the engine holds
/// back every service a failed-fast call belonged to (see
/// [`quota_abort_count`]) and lands the rest.
fn rate_limit_abort_error() -> AgentCallError {
    QUOTA_ABORTS.fetch_add(1, Ordering::Relaxed);
    AgentCallError {
        code: QUOTA_ABORT_CODE.to_string(),
        message: "Carrick Cloud LLM quota exhausted; failing fast. This is a rate/quota \
                  limit on the analysis backend, not a problem with the scanned code. The \
                  services this reaches are held back and named at the end of the scan; \
                  re-run after the quota resets."
            .to_string(),
        retriable: false,
    }
}

/// A failed lambda call, carrying the cloud's own transient/permanent verdict
/// so callers can classify a failure without pattern-matching on message text.
///
/// `retriable` is the envelope's `error.retriable` verbatim when the call
/// reached the lambda (the 429-wrapped 503 `model_error` the backend raises
/// under Vertex pressure is `retriable: true`); for failures that never
/// produced an envelope — bare network errors, unparseable gateway bodies —
/// the scanner fills in the equivalent verdict. An `Err` with `retriable: true`
/// therefore means "transient, and the backoff chain was already spent on it",
/// which is what lets a caller report failed-after-retry honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCallError {
    /// Cloud error code (`model_error`, `rate_limited`, `internal_error`), or a
    /// scanner-side pseudo-code for a failure that never reached the envelope.
    pub code: String,
    pub message: String,
    /// Whether the failure class is transient. See the type doc.
    pub retriable: bool,
}

impl AgentCallError {
    /// A permanent, non-retriable failure (server-side bug, malformed response).
    fn permanent(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
            retriable: false,
        }
    }

    /// A transient failure whose backoff chain has been spent.
    fn transient(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
            retriable: true,
        }
    }

    /// Whether this call failed because the process-global quota breaker is
    /// open rather than on its own merits. Such a call was never attempted, so
    /// counting it as a retry failure overstates the loss.
    pub fn is_quota_abort(&self) -> bool {
        self.code == QUOTA_ABORT_CODE
    }

    /// Whether the model was deliberately not asked, rather than asked and
    /// silent. A budget refusal is a fact about the account, not about the
    /// file, so the file is not lost and the run does not fail.
    pub fn is_budget_refusal(&self) -> bool {
        self.code == LLM_DISABLED_CODE
    }
}

impl std::fmt::Display for AgentCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Agent error '{}' (retriable={}): {}",
            self.code, self.retriable, self.message
        )
    }
}

impl std::error::Error for AgentCallError {}

/// Attempts per call: the initial try plus six backed-off retries.
const MAX_RETRIES: u32 = 7;
/// First backoff sleep; doubles each attempt.
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// Ceiling on a single backoff sleep (reached at attempt 6: 2→4→8→16→32→64s).
const RETRY_MAX_DELAY: Duration = Duration::from_secs(64);
/// Ceiling on a single sleep for a [`RetryPolicy::PATIENT`] call.
const PATIENT_RETRY_MAX_DELAY: Duration = Duration::from_secs(120);

/// How long one call keeps trying before it gives up.
///
/// Two policies, because the cost of giving up is not the same for every call.
/// A file analysis or an intent that fails costs one file or one function, and
/// the run carries on without it, so seven attempts over about two minutes is
/// the right amount of patience against a backend that may stay down. A call
/// the rest of a service depends on — framework detection, and the guidance
/// built from it — costs the whole service when it fails, and the failure it
/// usually meets is a shared model quota that refills within minutes
/// (2026-09-15: one detection call, seven `model_error` answers over 106 s,
/// and a seven-service first index aborted). A refused call costs nothing, so
/// those calls wait far longer before the engine defers the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in total, the first included.
    max_attempts: u32,
    /// Ceiling on one sleep, and on the `Retry-After` hint honoured.
    max_delay: Duration,
    /// Ceiling on the sleeps added together. A retry whose wait would cross it
    /// is not made.
    wait_budget: Duration,
    /// Whether this call's sleeps draw on the run-wide budget
    /// ([`crate::retry_budget`]) as well as its own.
    run_budgeted: bool,
}

impl RetryPolicy {
    /// Every per-file and per-function call.
    pub const STANDARD: Self = Self {
        max_attempts: MAX_RETRIES,
        max_delay: RETRY_MAX_DELAY,
        // Never the binding limit: the attempts run out first (~126 s at most).
        wait_budget: Duration::from_secs(3600),
        run_budgeted: false,
    };

    /// A call a whole service depends on: exponential with jitter, sleeps of
    /// up to two minutes, and up to ten minutes of sleeping before it fails,
    /// all of it drawn from the run's retry budget ([`crate::retry_budget`]).
    /// Once that is spent the call gets its first attempt and no sleep.
    pub const PATIENT: Self = Self {
        max_attempts: 32,
        max_delay: PATIENT_RETRY_MAX_DELAY,
        wait_budget: Duration::from_secs(600),
        run_budgeted: true,
    };

    /// Whether a failed attempt `attempt` may be followed by a sleep of `next`
    /// after `waited` has already been slept on this call.
    fn permits(&self, attempt: u32, waited: Duration, next: Duration) -> bool {
        attempt < self.max_attempts && waited + next <= self.wait_budget
    }

    /// [`Self::permits`], and for a run-budgeted policy, whether the run's
    /// budget still has room for `next`.
    fn permits_in_run(&self, attempt: u32, waited: Duration, next: Duration) -> bool {
        if !self.permits(attempt, waited, next) {
            return false;
        }
        if self.run_budgeted && !crate::retry_budget::fits(next) {
            debug!(
                "Not retrying: this run has spent its {}s retry budget ({}), so the call fails \
                 now",
                crate::retry_budget::budget().as_secs(),
                crate::retry_budget::BUDGET_ENV
            );
            return false;
        }
        true
    }

    /// Sleep before the next attempt, charged to the run's budget when the
    /// policy draws on it.
    async fn sleep(&self, duration: Duration) {
        if self.run_budgeted {
            crate::retry_budget::wait(duration).await;
        } else {
            sleep(duration).await;
        }
    }
}

/// Whether a failed call should consume a backoff attempt.
///
/// The cloud's `retriable` flag is the single source of truth for the
/// transient class — the scanner does not re-derive it from status codes or
/// message text. A quota abort is excluded even though quota errors are
/// nominally transient: quota does not clear inside one scan, and the breaker
/// has already decided that every remaining call fails fast.
fn should_retry(err: &AgentCallError, attempt: u32, max_retries: u32) -> bool {
    err.retriable && !err.is_quota_abort() && attempt < max_retries
}

/// Equal-jitter exponential backoff: half the exponential delay, plus a random
/// share of the other half.
///
/// Jitter is not cosmetic here. Up to `CARRICK_CONCURRENCY_LIMIT` workers hit
/// the same overloaded backend within milliseconds of each other, and an
/// unjittered `2^attempt` sleep makes them all wake in lockstep and re-fire as
/// one burst — the exact pattern that keeps a rate-limited backend rate
/// limited. Spreading each waker across half a window decorrelates them.
///
/// This is the standard equal-jitter formulation, so the previous unjittered
/// schedule (2, 4, 8, 16, 32, 64s) is now the *ceiling* of each window rather
/// than its midpoint: the worst-case chain is unchanged at ~126s, the mean
/// sleep is three quarters of what it was. Decorrelating the wakers is worth
/// more than the quarter-window of extra patience.
///
/// Pure so it can be tested: `jitter` is any value in `0..=u32::MAX`, supplied
/// by [`jitter_seed`] at the call site.
#[cfg(test)]
fn backoff_delay(attempt: u32, jitter: u32) -> Duration {
    backoff_delay_within(attempt, jitter, RETRY_MAX_DELAY)
}

/// [`backoff_delay`] under a policy's own ceiling.
fn backoff_delay_within(attempt: u32, jitter: u32, max_delay: Duration) -> Duration {
    let exponential = RETRY_BASE_DELAY
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(max_delay);
    let half = exponential / 2;
    let spread = half.mul_f64(f64::from(jitter) / f64::from(u32::MAX));
    half + spread
}

/// The longest `Retry-After` the scanner will read. A hint above it is read
/// as this, so a mistyped header on the cloud side cannot park a worker for an
/// hour. Each policy then caps the hint again at its own sleep ceiling, so a
/// per-file call still honours no more than [`RETRY_MAX_DELAY`].
const RETRY_AFTER_CAP: Duration = PATIENT_RETRY_MAX_DELAY;

/// The request header that numbers this call's attempts for the lambda
/// (carrick-cloud#875). The lambda gives attempt 1 its full in-lambda model
/// retry chain and every later attempt a single model try, because this loop
/// has already waited between rounds. Without it the two loops multiplied:
/// three lambda tries on each of seven scanner attempts, 21 model calls for one
/// function. With it the worst case is 3 + 6 = 9.
const ATTEMPT_HEADER: &str = "X-Carrick-Attempt";

/// A `Retry-After` value in delta-seconds, the only form the cloud sends. The
/// HTTP-date form and anything unparseable read as no hint, which leaves the
/// ordinary backoff in charge.
fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let seconds: u64 = value?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds).min(RETRY_AFTER_CAP))
}

/// How long to wait before the next attempt: the jittered backoff, or the
/// cloud's `Retry-After` hint when that is longer.
///
/// The hint is jittered too, by up to half again. A capacity dip at the model
/// provider answers every in-flight worker at once, all with the same hint,
/// and honouring it exactly would wake them in lockstep, which is the burst
/// [`backoff_delay`]'s jitter exists to prevent.
#[cfg(test)]
fn retry_wait(attempt: u32, jitter: u32, retry_after: Option<Duration>) -> Duration {
    retry_wait_within(attempt, jitter, retry_after, RETRY_MAX_DELAY)
}

/// [`retry_wait`] under a policy's own ceiling.
fn retry_wait_within(
    attempt: u32,
    jitter: u32,
    retry_after: Option<Duration>,
    max_delay: Duration,
) -> Duration {
    let backoff = backoff_delay_within(attempt, jitter, max_delay);
    let Some(hint) = retry_after else {
        return backoff;
    };
    let hint = hint.min(max_delay);
    let spread = (hint / 2).mul_f64(f64::from(jitter) / f64::from(u32::MAX));
    backoff.max(hint + spread)
}

/// How many lease waits ([`ANALYSIS_IN_FLIGHT_CODE`]) one call sits out
/// without spending an attempt. Each wait is a request the cloud held for
/// most of the gateway's 30 s before answering, plus a short sleep, and the
/// lease lives no longer than the holder's invocation (120 s). Four waits
/// outlast any holder; eight is twice that. Past the cap a wait is retried
/// like any retriable error, spending attempts, so a lease that never clears
/// (a cloud bug) still ends the call.
const MAX_IN_FLIGHT_WAITS: u32 = 8;

/// The sleep before collecting an in-flight answer when the reply carried no
/// `Retry-After`. Short on purpose: the cloud has already waited server-side,
/// and the next request waits again or finds the answer cached.
const IN_FLIGHT_DEFAULT_WAIT: Duration = Duration::from_secs(2);

/// How long to sleep before re-sending after a lease wait: the cloud's
/// `Retry-After` when it sent one, else [`IN_FLIGHT_DEFAULT_WAIT`], capped at
/// the policy's ceiling and jittered by up to half again. Not the exponential
/// backoff: no attempt was spent, and nothing is overloaded.
fn in_flight_wait(jitter: u32, retry_after: Option<Duration>, max_delay: Duration) -> Duration {
    let hint = retry_after.unwrap_or(IN_FLIGHT_DEFAULT_WAIT).min(max_delay);
    hint + (hint / 2).mul_f64(f64::from(jitter) / f64::from(u32::MAX))
}

/// Jitter source: the sub-second component of the wall clock. Enough entropy
/// to decorrelate wakers that are milliseconds apart, and avoids taking a
/// direct dependency on `rand` for a sleep length.
fn jitter_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Whether a response says the OIDC token was rejected.
///
/// A 401 is the direct signal. The indirect one matters just as much: the
/// gateway in front of the cloud can answer a rejected token with a 5xx and a
/// body that is not an error envelope, and that is indistinguishable from a
/// backend overload unless the body is read (#461). Bodies are only sniffed on
/// non-2xx responses, so an analysis result that quotes the code out of scanned
/// source can never be read as a rejection.
pub(crate) fn is_oidc_rejection(status: u16, body: &str) -> bool {
    status == 401 || (!(200..300).contains(&status) && body.contains("oidc_invalid"))
}

/// A short, single-line excerpt of a response body, for a log line or an error
/// message. Bodies can be large and can carry newlines; neither belongs in a
/// warning.
fn body_excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_CHARS {
        let head: String = collapsed.chars().take(MAX_CHARS).collect();
        format!("{}...", head)
    } else if collapsed.is_empty() {
        "<empty body>".to_string()
    } else {
        collapsed
    }
}

/// The credential one prompt-lambda call sends, resolved.
///
/// [`crate::credentials::CloudAuth`] is the process's answer to "which
/// credential do I hold"; this is the same answer with the OIDC provider
/// already looked up, so the retry loop can be driven against a stub token
/// endpoint in tests rather than the process global.
enum RequestAuth<'a> {
    Oidc(&'a OidcProvider),
    Bearer(String),
}

/// Lambda calls in flight across the whole process when
/// `CARRICK_CONCURRENCY_LIMIT` is unset.
///
/// Sized for two stages at once. A service's file analysis queues up to
/// [`crate::agents::file_orchestrator::FILE_ANALYSIS_QUEUE_DEPTH`] calls and its
/// function intents up to `DEFAULT_INTENT_CONCURRENCY`, and the intent stage
/// now starts at discovery instead of after the last file (carrick#1065). At
/// 20 the intents would only get the slots file analysis left free, which
/// during its burst is none; 28 lets both stages progress while keeping the
/// burst against the model backend well under the sum of the two queues.
const DEFAULT_CONCURRENCY_LIMIT: usize = 28;

/// `CARRICK_CONCURRENCY_LIMIT`, or [`DEFAULT_CONCURRENCY_LIMIT`]: the ceiling
/// on lambda calls in flight across the process. The per-stage queue depths
/// only say how eagerly a stage queues work; this is what bounds the requests
/// on the wire.
fn concurrency_limit() -> usize {
    env::var("CARRICK_CONCURRENCY_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_CONCURRENCY_LIMIT)
        .max(1)
}

/// The one semaphore every `AgentService` draws its permits from.
///
/// Process-global for the same reason as [`RATE_LIMITED`]: a scan constructs
/// several `AgentService` instances (detection, file analysis, intents), and
/// with a semaphore per instance two stages running side by side would put
/// the SUM of their limits on the wire. The limit is read once, on first use.
///
/// A permit covers one HTTP attempt, not a whole call: a call asleep between
/// retries holds none and queues for one again when it wakes (carrick#1077).
/// So the cap bounds requests on the wire, and the calls waiting out a
/// backoff are bounded separately, by the stage queue depths.
fn global_semaphore() -> Arc<Semaphore> {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(concurrency_limit())))
        .clone()
}

/// The adaptive limit per cloud route, inside the process-wide cap
/// (carrick-cloud#869). Each route starts at [`concurrency_limit`], halves
/// when the cloud refuses a request for capacity and climbs back by one after
/// a run of successes; see `limiter.rs`. Process-global for the same reason as
/// the semaphore: a busy model is busy for every `AgentService` that asks it.
fn global_route_limits() -> Arc<RouteLimits> {
    static LIMITS: OnceLock<Arc<RouteLimits>> = OnceLock::new();
    LIMITS
        .get_or_init(|| Arc::new(RouteLimits::new(concurrency_limit())))
        .clone()
}

/// The request rate across every route, paced to the gateway throttle
/// (carrick-cloud#869). Unpaced until a gateway 429; see `limiter.rs`.
fn global_pacer() -> Arc<RatePacer> {
    static PACER: OnceLock<Arc<RatePacer>> = OnceLock::new();
    PACER.get_or_init(|| Arc::new(RatePacer::new())).clone()
}

/// Whether a retriable error ENVELOPE says the route's model is out of
/// capacity. The lambda answers 503 `model_error` with a `Retry-After` when
/// the model's own retries ran out, and that is per model, so it cuts the
/// route's concurrency. A 429 envelope never reaches here (`rate_limited`
/// trips the breaker first), and anything else retriable says nothing about
/// capacity. Neither does a lease wait, whatever its status and headers, so
/// callers rule [`is_analysis_in_flight`] out first (carrick#1131).
fn is_model_busy(status: u16, retry_after: Option<Duration>) -> bool {
    status == 503 || retry_after.is_some()
}

/// Whether a response with NO envelope is the API Gateway's throttle. The
/// gateway answers 429 before any lambda runs, for every route alike, so it is
/// a request-rate signal for the pacer and not a verdict on any model. A
/// non-envelope 502/503/504 may be a lambda timeout, which says nothing about
/// either.
fn is_gateway_throttle(status: u16) -> bool {
    status == 429
}

/// Reusable service for making Agent API calls
#[derive(Debug, Clone)]
pub struct AgentService {
    client: Client,
    semaphore: Arc<Semaphore>,
    limits: Arc<RouteLimits>,
    pacer: Arc<RatePacer>,
    retry: RetryPolicy,
}

impl AgentService {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let use_system_proxy = env::var("CARRICK_USE_SYSTEM_PROXY").is_ok();
        let mut client_builder = Client::builder();
        if !use_system_proxy {
            client_builder = client_builder.no_proxy();
        }
        let client = client_builder
            .build()
            .expect("Failed to build agent HTTP client");

        Self {
            client,
            semaphore: global_semaphore(),
            limits: global_route_limits(),
            pacer: global_pacer(),
            retry: RetryPolicy::STANDARD,
        }
    }

    /// The same service, retrying under `policy` (see [`RetryPolicy`]).
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Per-task lambda call where the lambda just needs a user_message +
    /// schema (e.g. file-analyzer). The lambda owns the system prompt.
    /// `task_path` is the API Gateway route, e.g. "/analyze-file".
    ///
    /// `guidance` names the guidance block the prompt embeds and says how many
    /// of its leading bytes that block occupies, so the cloud's analysis cache
    /// can key on the guidance's ID instead of its TEXT (carrick-cloud#871).
    /// It is key material only: what reaches the model is `user_message` and
    /// nothing else, and a `None` here (an offline run, a guidance answer that
    /// carried no key) keys on the whole message exactly as before.
    pub async fn analyze_with_lambda(
        &self,
        task_path: &str,
        user_message: &str,
        response_schema: Option<serde_json::Value>,
        guidance: Option<GuidanceRef<'_>>,
    ) -> Result<String, AgentCallError> {
        let request = LambdaRequest {
            user_message: user_message.to_string(),
            response_schema,
            guidance_key: guidance.map(|g| g.key.to_string()),
            guidance_prefix_bytes: guidance.map(|g| g.prefix_bytes),
        };
        self.post_to_lambda(task_path, &request, user_message).await
    }

    /// Lower-level per-task lambda call for arbitrary structured payloads
    /// (e.g. framework-guidance which sends task+category+frameworks).
    /// `mock_seed` is used in mock mode to pick the right canned response.
    pub async fn post_to_lambda<B: Serialize + ?Sized>(
        &self,
        task_path: &str,
        body: &B,
        mock_seed: &str,
    ) -> Result<String, AgentCallError> {
        self.post_to_lambda_keyed(task_path, body, mock_seed)
            .await
            .map(|outcome| outcome.text)
    }

    /// As [`Self::post_to_lambda`], but keeping the envelope fields that sit
    /// beside `text`. Only framework-guidance has one today (`guidance_key`),
    /// and only the file analyzer needs it — every other caller wants the text
    /// and uses the wrapper above.
    pub async fn post_to_lambda_keyed<B: Serialize + ?Sized>(
        &self,
        task_path: &str,
        body: &B,
        mock_seed: &str,
    ) -> Result<LambdaOutcome, AgentCallError> {
        // Counted here, before the mock short-circuit: this is every request
        // the scan would put on the wire. The concurrency permit is taken per
        // HTTP attempt inside `post_with_retry`, so an offline run takes none.
        record_request(task_path);

        if env::var("CARRICK_MOCK_ALL").is_ok() {
            if let Some(error) = take_mock_failure(task_path, body) {
                return Err(error);
            }
            return Ok(LambdaOutcome {
                text: generate_mock_for_task(task_path, body, mock_seed),
                guidance_key: None,
            });
        }

        // Which credential this process holds, read per call rather than
        // cached, because it is cheap and the whole point of the branch is
        // that a laptop and a runner never both apply. A laptop call carries
        // `Authorization` and the scan slot; a runner call carries the OIDC
        // header, exactly as it did.
        let auth = match crate::credentials::CloudAuth::detect()
            .map_err(|e| AgentCallError::permanent("oidc_unavailable", e))?
        {
            crate::credentials::CloudAuth::Oidc => RequestAuth::Oidc(
                OidcProvider::global()
                    .map_err(|e| AgentCallError::permanent("oidc_unavailable", e.to_string()))?,
            ),
            crate::credentials::CloudAuth::Bearer(token) => RequestAuth::Bearer(token),
        };
        self.post_with_retry(&auth, env!("CARRICK_API_ENDPOINT"), task_path, body)
            .await
    }

    /// Shared HTTP + retry implementation for all lambda calls. Sends
    /// the version header, parses the structured error envelope, and
    /// only consumes a backoff attempt when the error is marked
    /// retriable=true (or on bare network failures).
    ///
    /// `auth` and `api_base` are parameters rather than the globals the
    /// public entry point reads, so the retry loop can be driven against a
    /// stub in tests.
    async fn post_with_retry<B>(
        &self,
        auth: &RequestAuth<'_>,
        api_base: &str,
        path: &str,
        body: &B,
    ) -> Result<LambdaOutcome, AgentCallError>
    where
        B: Serialize + ?Sized,
    {
        let endpoint = format!("{}{}", api_base, path);

        // Whether this call has already answered a rejected token with a fresh
        // mint. A second rejection after that is not an expiry, so it is fatal
        // rather than something to keep retrying with the same credential.
        let mut reminted = false;

        // The attempt number the lambda sees in `X-Carrick-Attempt`. Counted
        // separately from the loop index: a re-mint re-sends without the model
        // ever having been asked, so it must not cost the next request its
        // full in-lambda chain.
        let mut lambda_attempt: u32 = 1;

        // Retry logic for transient failures with jittered exponential
        // backoff. 7 attempts, sleeps halving-jittered around 2s, 4s, 8s, 16s,
        // 32s, 64s (see `backoff_delay`). The lambda's structured error
        // envelope (`error.retriable`) is the source of truth for
        // application-level errors. We additionally retry on transient
        // *gateway* errors (429/502/503/504) where the body may not
        // even be a parseable JSON envelope (API Gateway timeouts return
        // non-envelope responses). A `Retry-After` on either kind of
        // response is a floor under the backoff (see `retry_wait`), and each
        // request numbers itself in `X-Carrick-Attempt` so the lambda sizes
        // its own model retries to this loop (carrick-cloud#875).
        let policy = self.retry;
        let max_retries = policy.max_attempts;
        // What this call has slept so far, against the policy's wait budget.
        let mut waited = Duration::ZERO;
        let route_limit = self.limits.for_route(path);
        // Lease waits sat out so far, against [`MAX_IN_FLIGHT_WAITS`]. Counted
        // by hand, with `attempt`, so a wait can hand its attempt back.
        let mut in_flight_waits: u32 = 0;
        let mut attempt: u32 = 0;
        while attempt < max_retries {
            attempt += 1;
            // A sibling call (any phase, any `AgentService`) may have already
            // hit the backend quota wall. Re-checked each attempt so a worker
            // mid-backoff aborts after its current sleep instead of firing a
            // doomed request that burns more quota.
            if rate_limit_tripped() {
                return Err(rate_limit_abort_error());
            }

            // Read the token per attempt, not once per call. A scan of a
            // large repo outlives a token, and the provider mints a fresh one
            // as soon as the cached one nears its expiry — so the retry that
            // happens ten minutes into a call chain carries a valid credential
            // instead of the one this call started with (#461). A Bearer
            // credential is long-lived and unchanged between attempts, but it
            // is read the same way so the two branches differ only in the
            // header they set.
            let token = match auth {
                RequestAuth::Oidc(provider) => provider
                    .token()
                    .await
                    .map_err(|e| AgentCallError::permanent("oidc_unavailable", e.to_string()))?,
                RequestAuth::Bearer(token) => token.clone(),
            };

            // One slot per HTTP attempt, not per call (carrick#1077). The
            // permit covers the send and the body read, and is dropped before
            // every retry sleep: a call waiting out a 10-15 s `Retry-After`
            // is not a request on the wire, so it must not idle a slot another
            // call could use. It re-queues for a slot when it wakes. Taken
            // after the token read, so minting a token holds no slot either.
            //
            // Two slots, in this order: the route's adaptive one, then the
            // process-wide one. The other order would idle a process-wide
            // slot while this attempt queues behind a busy route. Each slot
            // is released at the same points; the route slot also carries the
            // attempt's verdict back to its limit (carrick-cloud#869).
            let route_slot = route_limit.acquire().await;
            let permit = self.semaphore.acquire().await.map_err(|e| {
                AgentCallError::permanent(
                    "semaphore_closed",
                    format!("Failed to acquire semaphore permit: {}", e),
                )
            })?;

            // Paced last, holding both slots, so the reserved time is the
            // time the request goes. A wait here is the throttle working, not
            // a retry sleep: while the gateway is rate-limiting the scan, the
            // rate is what bounds the stage, and the slots are not idle so
            // much as queued behind it.
            let reservation = self.pacer.reserve();
            tokio::time::sleep_until(reservation.at).await;

            let mut request_builder = self
                .client
                .post(&endpoint)
                .json(body)
                .timeout(std::time::Duration::from_secs(60))
                .header("X-Carrick-Scanner-Version", env!("CARGO_PKG_VERSION"))
                .header("X-Carrick-Run-Id", crate::logging::run_id())
                .header(ATTEMPT_HEADER, lambda_attempt.to_string());
            request_builder = match auth {
                RequestAuth::Oidc(_) => request_builder.header("X-Carrick-OIDC", &token),
                RequestAuth::Bearer(_) => {
                    // The scan slot rides every prompt call of a laptop run:
                    // it is how the cloud knows which repo is spending, and the
                    // money gates read the repo out of the slot rather than out
                    // of anything this client asserts (C4). Absent on the CI
                    // path, where none was minted.
                    let builder =
                        request_builder.header("Authorization", format!("Bearer {token}"));
                    match crate::credentials::scan_id() {
                        Some(scan_id) => builder.header("X-Carrick-Scan-Id", scan_id),
                        None => builder,
                    }
                }
            };

            match request_builder.send().await {
                Ok(response) => {
                    let status = response.status();
                    // Read before the body consumes the response.
                    let retry_after = parse_retry_after(
                        response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok()),
                    );

                    // Read the body as text once. Going through `.json()`
                    // discarded it, so a non-envelope response was logged as
                    // "error decoding response body" with no trace of what the
                    // body said — which is how an auth rejection wrapped in a
                    // gateway status read as a plain overload (#461).
                    let response_text = match response.text().await {
                        Ok(text) => text,
                        Err(e) => {
                            // The response never arrived in full: transport, not
                            // application, so it is retriable by definition.
                            let wait_time =
                                backoff_delay_within(attempt, jitter_seed(), policy.max_delay);
                            if policy.permits_in_run(attempt, waited, wait_time) {
                                note_retry();
                                warn!(
                                    target: crate::logging::RETRY_TARGET,
                                    "Failed to read agent proxy response ({}): {}. Retrying in {:?} (attempt {}/{})",
                                    status, e, wait_time, attempt, max_retries
                                );
                                drop(permit);
                                drop(route_slot);
                                policy.sleep(wait_time).await;
                                waited += wait_time;
                                lambda_attempt += 1;
                                continue;
                            }
                            return Err(AgentCallError::transient(
                                "network_error",
                                format!("Failed to read agent proxy response {}: {}", status, e),
                            ));
                        }
                    };

                    // An expired token is not always a 401 at the client: the
                    // gateway can wrap the rejection in a 5xx whose body is not
                    // an envelope, which the retry loop then spends its whole
                    // budget on as if it were an overload. Read the rejection
                    // out of the body as well as the status, mint a fresh
                    // token, and retry immediately. Only non-2xx bodies are
                    // sniffed, so a successful analysis of a file that happens
                    // to mention the code is never mistaken for one.
                    if is_oidc_rejection(status.as_u16(), &response_text) {
                        // A Bearer credential cannot be re-minted: the only
                        // thing that replaces it is a fresh consent in a
                        // browser. So the rejection is stated and the call
                        // stops, rather than spending a retry on the same
                        // token (§8.2). `wrong_key_kind` arrives here too,
                        // which is what an `mcp`-scoped credential gets until
                        // the user logs in again.
                        let RequestAuth::Oidc(provider) = auth else {
                            // A Bearer credential cannot be re-minted: only a
                            // fresh consent replaces it, so this call stops
                            // rather than spending a retry on the same token
                            // (§8.2). `is_oidc_rejection` matches 401 and the
                            // kind gate; every other refusal on this path
                            // (`scan_not_started`, a spend cap) falls through
                            // to the envelope below and is read as what it is.
                            return Err(AgentCallError::permanent(
                                "credential_rejected",
                                format!(
                                    "Carrick Cloud rejected this credential (status {}): {}. \
                                     Run carrick login and try again.",
                                    status,
                                    body_excerpt(&response_text)
                                ),
                            ));
                        };
                        if reminted {
                            return Err(AgentCallError::permanent(
                                "oidc_rejected",
                                format!(
                                    "Agent proxy rejected a freshly minted OIDC token (status {}): {}.                                      The scan cannot authenticate to Carrick Cloud.",
                                    status,
                                    body_excerpt(&response_text)
                                ),
                            ));
                        }
                        warn!(
                            "Agent proxy rejected the OIDC token (status {}: {}); re-minting and retrying",
                            status,
                            body_excerpt(&response_text)
                        );
                        // Minting is a round trip to the token endpoint, not
                        // to the cloud, so it holds no slot.
                        drop(permit);
                        drop(route_slot);
                        provider.remint(&token).await.map_err(|e| {
                            AgentCallError::permanent("oidc_unavailable", e.to_string())
                        })?;
                        reminted = true;
                        continue;
                    }

                    let is_transient_gateway_status =
                        matches!(status.as_u16(), 429 | 502 | 503 | 504);

                    let body: AgentResponse = match serde_json::from_str(&response_text) {
                        Ok(b) => b,
                        Err(e) => {
                            // Body wasn't a parseable envelope. If the status
                            // is a known transient gateway code, retry —
                            // otherwise fail fast (server-side bug).
                            let wait_time = retry_wait_within(
                                attempt,
                                jitter_seed(),
                                retry_after,
                                policy.max_delay,
                            );
                            if is_transient_gateway_status
                                && policy.permits_in_run(attempt, waited, wait_time)
                            {
                                let line = format!(
                                    "Gateway status {} with non-envelope body ({}): {}. Retrying in {:?} (attempt {}/{})",
                                    status,
                                    e,
                                    body_excerpt(&response_text),
                                    wait_time,
                                    attempt,
                                    max_retries
                                );
                                drop(permit);
                                drop(route_slot);
                                if is_gateway_throttle(status.as_u16()) {
                                    // The terminal hears about the throttle
                                    // once, from the pacer; each 429 is
                                    // file-log detail.
                                    debug!("{line}");
                                    self.pacer.throttled(reservation.epoch);
                                    // The gateway refused before any lambda
                                    // ran, so the model was never asked: the
                                    // re-send keeps its attempt number and
                                    // the lambda's full chain, as a re-mint
                                    // does (carrick-cloud#875).
                                } else {
                                    // A 502/503/504 may be a lambda that timed
                                    // out mid-call, so the model may have been
                                    // asked. Each attempt is file-log detail
                                    // too, with one counted terminal line.
                                    note_retry();
                                    warn!(target: crate::logging::RETRY_TARGET, "{line}");
                                    lambda_attempt += 1;
                                }
                                policy.sleep(wait_time).await;
                                waited += wait_time;
                                continue;
                            }
                            // Out of attempts: the throttle still says the
                            // scan is sending too fast.
                            if is_gateway_throttle(status.as_u16()) {
                                self.pacer.throttled(reservation.epoch);
                            }
                            let message = format!(
                                "Agent proxy returned status {} with unparseable body ({}): {}",
                                status,
                                e,
                                body_excerpt(&response_text)
                            );
                            return Err(if is_transient_gateway_status {
                                AgentCallError::transient("gateway_error", message)
                            } else {
                                AgentCallError::permanent("bad_response", message)
                            });
                        }
                    };

                    if status.is_success() && body.success {
                        drop(permit);
                        route_slot.succeeded();
                        self.pacer.admitted();
                        return Ok(LambdaOutcome {
                            text: body.text.unwrap_or_default(),
                            guidance_key: body.guidance_key,
                        });
                    }

                    let err = match body.error {
                        Some(err) => err,
                        None => {
                            return Err(AgentCallError::permanent(
                                "bad_response",
                                format!(
                                    "Agent proxy status {} success={} but no error envelope",
                                    status, body.success
                                ),
                            ));
                        }
                    };

                    // A quota / rate-limit error will not clear within a single
                    // scan, and each retry consumes more of the exhausted
                    // budget. Trip the process-global breaker so sibling
                    // workers abort fast instead of each grinding the full
                    // backoff chain, and fail this call now.
                    if is_quota_error(&err) {
                        trip_rate_limit();
                        warn!(
                            "Backend LLM quota exhausted ({}); tripping circuit breaker — remaining calls fail fast, and the services they belong to are held back and retried once before the scan ends",
                            err.message
                        );
                        return Err(rate_limit_abort_error());
                    }

                    // The lease wait (carrick#1131): an earlier request for
                    // this prompt is still being analysed. Named by its own
                    // code whichever wire shape carried it, so a call that
                    // does run out says what it waited on.
                    let in_flight = is_analysis_in_flight(&err);
                    let call_err = AgentCallError {
                        code: if in_flight {
                            ANALYSIS_IN_FLIGHT_CODE.to_string()
                        } else {
                            err.code
                        },
                        message: err.message,
                        retriable: err.retriable,
                    };

                    // Sat out as a pure wait: both slots released with no
                    // verdict, as a gateway timeout releases them, the
                    // cloud's Retry-After honoured, and the attempt handed
                    // back. The model was not asked, so `X-Carrick-Attempt`
                    // does not advance either: a re-send that finds the lease
                    // gone and becomes the holder keeps the chain it had.
                    if in_flight && call_err.retriable && in_flight_waits < MAX_IN_FLIGHT_WAITS {
                        in_flight_waits += 1;
                        let wait_time =
                            in_flight_wait(jitter_seed(), retry_after, policy.max_delay);
                        debug!(
                            "An earlier request for this {} call is still being analysed; \
                             collecting its answer in {:?} (wait {}/{})",
                            path, wait_time, in_flight_waits, MAX_IN_FLIGHT_WAITS
                        );
                        drop(permit);
                        drop(route_slot);
                        sleep(wait_time).await;
                        attempt -= 1;
                        continue;
                    }
                    // Past the cap, a lease wait is retried like any retriable
                    // error below, but it is still no verdict on capacity.
                    let model_busy = !in_flight && is_model_busy(status.as_u16(), retry_after);

                    // The cloud's Retry-After on a 503 `model_error` is the
                    // floor: re-firing after one or two seconds lands in the
                    // same capacity dip the lambda just gave up on.
                    let wait_time =
                        retry_wait_within(attempt, jitter_seed(), retry_after, policy.max_delay);
                    if should_retry(&call_err, attempt, max_retries)
                        && policy.permits_in_run(attempt, waited, wait_time)
                    {
                        let line = format!(
                            "Agent error '{}' is retriable, retrying in {:?} (attempt {}/{}): {}",
                            call_err.code, wait_time, attempt, max_retries, call_err.message
                        );
                        drop(permit);
                        if model_busy {
                            // One aggregated terminal line comes from the
                            // limiter; each refusal is file-log detail.
                            debug!("{line}");
                            route_slot.overloaded();
                        } else {
                            note_retry();
                            warn!(target: crate::logging::RETRY_TARGET, "{line}");
                            drop(route_slot);
                        }
                        policy.sleep(wait_time).await;
                        waited += wait_time;
                        lambda_attempt += 1;
                        continue;
                    }

                    // Out of attempts, the refusal is still a verdict on the
                    // model: the call that waited longest must not be the one
                    // the limit never hears about.
                    if call_err.retriable && model_busy {
                        route_slot.overloaded();
                    }
                    return Err(call_err);
                }
                Err(e) => {
                    // Bare network failure (no response received) — retriable by definition.
                    let wait_time = backoff_delay_within(attempt, jitter_seed(), policy.max_delay);
                    if policy.permits_in_run(attempt, waited, wait_time) {
                        note_retry();
                        warn!(
                            target: crate::logging::RETRY_TARGET,
                            "Agent proxy network error: {}, retrying in {:?} (attempt {}/{})",
                            e, wait_time, attempt, max_retries
                        );
                        drop(permit);
                        drop(route_slot);
                        policy.sleep(wait_time).await;
                        waited += wait_time;
                        lambda_attempt += 1;
                        continue;
                    }

                    return Err(AgentCallError::transient(
                        "network_error",
                        format!("Agent proxy call failed: {}", e),
                    ));
                }
            }
        }

        Err(AgentCallError::transient(
            "retries_exhausted",
            "Maximum retry attempts exceeded".to_string(),
        ))
    }
}

/// A failure an offline run answers instead of the mock response, for the
/// tests that prove a scan survives a call the cloud never answered.
struct MockFailure {
    task_path: String,
    body_contains: String,
    remaining: usize,
    /// `model_error` (transient) or `llm_disabled` (a budget refusal).
    code: &'static str,
}

fn mock_failures() -> &'static Mutex<Vec<MockFailure>> {
    static FAILURES: OnceLock<Mutex<Vec<MockFailure>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Make the next `times` offline calls to `task_path` whose serialized body
/// contains `body_contains` fail the way a call whose retries were spent on an
/// overloaded model fails: `model_error`, `retriable: true`.
///
/// Honoured only under `CARRICK_MOCK_ALL`, where nothing reaches the cloud,
/// and matched on the body so a test can fail one service's call and not its
/// siblings' (framework detection is the same route for every service). The
/// retry loop is not run: the error is what that loop returns once it is spent.
#[allow(dead_code)] // Called by tests/ through the library, never by the binary.
pub fn inject_mock_failure(task_path: &str, body_contains: &str, times: usize) {
    mock_failures().lock().unwrap().push(MockFailure {
        task_path: task_path.to_string(),
        body_contains: body_contains.to_string(),
        remaining: times,
        code: "model_error",
    });
}

/// The same, answered the way a spent allowance answers: `llm_disabled`,
/// `retriable: false`. For the tests that pin what a refused service does to
/// the scan (carrick-cloud#892).
#[allow(dead_code)] // Called by tests/ through the library, never by the binary.
pub fn inject_mock_budget_refusal(task_path: &str, body_contains: &str, times: usize) {
    mock_failures().lock().unwrap().push(MockFailure {
        task_path: task_path.to_string(),
        body_contains: body_contains.to_string(),
        remaining: times,
        code: LLM_DISABLED_CODE,
    });
}

fn take_mock_failure<B: Serialize + ?Sized>(task_path: &str, body: &B) -> Option<AgentCallError> {
    let mut failures = mock_failures().lock().unwrap();
    if failures.is_empty() {
        return None;
    }
    let serialized = serde_json::to_string(body).unwrap_or_default();
    let failure = failures.iter_mut().find(|f| {
        f.remaining > 0 && f.task_path == task_path && serialized.contains(&f.body_contains)
    })?;
    failure.remaining -= 1;
    if failure.code == LLM_DISABLED_CODE {
        return Some(AgentCallError::permanent(
            LLM_DISABLED_CODE,
            "The allowance for this scan is spent (injected offline refusal)".to_string(),
        ));
    }
    Some(AgentCallError::transient(
        "model_error",
        "Gemini overloaded; retries exhausted (injected offline failure)".to_string(),
    ))
}

/// Request body for per-task lambda endpoints (e.g. /analyze-file).
/// The lambda owns the system prompt; Rust just sends the user payload.
#[derive(Debug, Serialize)]
struct LambdaRequest {
    user_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
    /// Key material for the cloud's analysis cache, never prompt content
    /// (carrick-cloud#871). Omitted entirely when absent so the request bytes
    /// an older cloud sees are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance_prefix_bytes: Option<usize>,
}

/// Lambda response envelope. On success: `success=true, text="..."`.
/// On failure: `success=false, error=AgentError{...}`. The `retriable`
/// flag on the error is the source of truth for whether the scanner
/// should consume an exponential-backoff attempt.
#[derive(Debug, Deserialize)]
struct AgentResponse {
    success: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<AgentError>,
    /// framework-guidance only: the id of the stored guidance entry this text
    /// came from. Absent from every other lambda's envelope, and absent from
    /// framework-guidance's own when its cache is disabled — hence `default`,
    /// and hence every consumer treating `None` as "no split available"
    /// (carrick-cloud#871).
    #[serde(default)]
    guidance_key: Option<String>,
}

/// What a lambda answered: the text, plus the envelope fields a caller may
/// need beside it.
#[derive(Debug, Clone)]
pub struct LambdaOutcome {
    pub text: String,
    pub guidance_key: Option<String>,
}

/// The guidance block a file prompt embeds, as the analyzer hands it to the
/// cloud: which stored guidance it is, and how many leading utf-8 bytes of the
/// user message it occupies. Bytes, not characters — the cloud slices the
/// utf-8 buffer (carrick-cloud#871).
#[derive(Debug, Clone, Copy)]
pub struct GuidanceRef<'a> {
    pub key: &'a str,
    pub prefix_bytes: usize,
}

#[derive(Debug, Deserialize, Clone)]
struct AgentError {
    code: String,
    message: String,
    retriable: bool,
    /// Whatever extra context the cloud attached: a `requestId` to quote, and
    /// on some codes a `reason` that says which of several causes it was
    /// (`llm_disabled`, the lease wait). Absent on most envelopes, and never a
    /// fixed shape, so it stays JSON and is read through accessors.
    #[serde(default)]
    details: Option<serde_json::Value>,
}

impl AgentError {
    /// `details.reason`, when the cloud sent one as a string.
    fn reason(&self) -> Option<&str> {
        self.details.as_ref()?.get("reason")?.as_str()
    }
}

/// The mock `/generate-intent` answer: the canned sentence for a single
/// request, and the same sentence for every function of a batched one, in the
/// lambda's batched `text` shape (carrick#1064).
fn mock_intent_answer<B: Serialize + ?Sized>(body: &B) -> String {
    const MOCK_INTENT: &str = "Mock intent: function does something.";
    let functions = serde_json::to_value(body).ok().and_then(|v| {
        v.get("functions")
            .and_then(|f| f.as_array())
            .map(|functions| {
                functions
                    .iter()
                    .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect::<Vec<_>>()
            })
    });
    match functions {
        Some(names) => serde_json::json!({
            "intents": names
                .iter()
                .map(|name| serde_json::json!({"name": name, "intent": MOCK_INTENT, "cached": false}))
                .collect::<Vec<_>>()
        })
        .to_string(),
        None => MOCK_INTENT.to_string(),
    }
}

/// Mock-mode dispatch by task path. Some lambdas don't send a
/// `response_schema` (e.g. /generate-intent ships only `{name, body,
/// called_intents}`), so falling through to schema-based dispatch
/// produces the wrong shape. This wrapper handles those tasks
/// explicitly before delegating to the generic schema-based mock.
fn generate_mock_for_task<B: Serialize + ?Sized>(
    task_path: &str,
    body: &B,
    mock_seed: &str,
) -> String {
    if let Some(canned) = fixture_mock_response(task_path, mock_seed) {
        return canned;
    }
    match task_path {
        "/generate-intent" => mock_intent_answer(body),
        _ => {
            // Tasks that send a schema (file-analyzer, framework-guidance)
            // dispatch by inspecting the schema shape. Tasks that don't but
            // happen to want the framework-detection-shaped fallback
            // (framework-detect) also land here — that's fine because the
            // default response_schema=None branch returns exactly that.
            let schema = serde_json::to_value(body)
                .ok()
                .and_then(|v| v.get("response_schema").cloned());
            generate_mock_response(&schema, mock_seed)
        }
    }
}

/// Fixture-driven mock responses for integration tests.
///
/// When `CARRICK_MOCK_FIXTURE_DIR` is set (alongside `CARRICK_MOCK_ALL`),
/// look up a canned response at `<dir>/<task>/<file_stem>.json`, keyed by the
/// analyzed file's path parsed from the user message. This lets tests replay
/// realistic agent output — including its known imperfections — through the
/// full sanitize/validate/mount-graph pipeline. Falls back to the schema-based
/// generated mocks when no fixture exists for the task/file.
fn fixture_mock_response(task_path: &str, mock_seed: &str) -> Option<String> {
    let dir = env::var("CARRICK_MOCK_FIXTURE_DIR").ok()?;
    let task = task_path.trim_start_matches('/');
    let marker = "### FILE CONTENT (Path: ";
    let key = match mock_seed.find(marker) {
        Some(idx) => {
            let rest = &mock_seed[idx + marker.len()..];
            let path = rest.split(')').next()?;
            std::path::Path::new(path)
                .file_stem()?
                .to_string_lossy()
                .into_owned()
        }
        // Tasks without a file in the prompt (framework-guidance) seed with a
        // short category token ("mount", "extraction_config", ...); use it as
        // the fixture key so one fixture dir can serve multiple tasks.
        None if is_fixture_key_token(mock_seed) => mock_seed.to_string(),
        None => "default".to_string(),
    };
    let fixture_path = std::path::Path::new(&dir)
        .join(task)
        .join(format!("{}.json", key));
    let canned = std::fs::read_to_string(&fixture_path).ok()?;
    debug!(
        "Mock fixture hit for {}: {}",
        task_path,
        fixture_path.display()
    );
    Some(substitute_candidate_placeholders(&canned, mock_seed))
}

/// A mock seed usable directly as a fixture file stem: short and free of
/// path/glob characters. Long seeds (full user messages) fall back to
/// `default`.
fn is_fixture_key_token(seed: &str) -> bool {
    !seed.is_empty()
        && seed.len() <= 64
        && seed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Replace `"@line:<n>"` candidate-id placeholders in a canned response with
/// the real SWC candidate id for that line, parsed from the prompt's
/// candidate hints (`- Candidate span:<a>-<b>: Line <n> ...`). This mirrors
/// the real agent contract — the LLM echoes the candidate_id it sees in the
/// CANDIDATE TARGETS section — without fixtures having to hard-code byte
/// offsets. Placeholders for lines with no candidate are left as-is, so they
/// fail the candidate map exactly like a hallucinated candidate_id would.
fn substitute_candidate_placeholders(canned: &str, mock_seed: &str) -> String {
    let mut out = canned.to_string();
    for line in mock_seed.lines() {
        let Some(rest) = line.trim_start().strip_prefix("- Candidate ") else {
            continue;
        };
        let Some((id, after)) = rest.split_once(": Line ") else {
            continue;
        };
        let line_no: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if line_no.is_empty() {
            continue;
        }
        out = out.replace(&format!("\"@line:{}\"", line_no), &format!("\"{}\"", id));
    }
    out
}

/// Generate mock response based on schema type
fn generate_mock_response(schema: &Option<serde_json::Value>, prompt: &str) -> String {
    match schema {
        Some(schema_val) => {
            // Check if schema is for an array
            if schema_val.get("type").and_then(|t| t.as_str()) == Some("ARRAY") {
                // Check what kind of array based on the items schema
                if let Some(items) = schema_val.get("items")
                    && let Some(props) = items.get("properties")
                {
                    // Triage schema - has location, classification, confidence
                    if props.get("classification").is_some() {
                        return generate_mock_triage_response(prompt);
                    }
                    // Endpoint schema - has method, path, handler, node_name
                    if props.get("node_name").is_some() && props.get("path").is_some() {
                        return generate_mock_endpoint_response(prompt);
                    }
                    // Consumer schema - has library, url, method
                    if props.get("library").is_some() {
                        return generate_mock_consumer_response(prompt);
                    }
                    // Mount schema - has parent_node, child_node, mount_path
                    if props.get("parent_node").is_some() && props.get("child_node").is_some() {
                        return generate_mock_mount_response(prompt);
                    }
                    // Middleware schema - has middleware_type
                    if props.get("middleware_type").is_some() {
                        return generate_mock_middleware_response(prompt);
                    }
                }
                // Default array response
                "[]".to_string()
            } else if schema_val.get("type").and_then(|t| t.as_str()) == Some("OBJECT") {
                if let Some(props) = schema_val.get("properties") {
                    // Check for file_analysis_schema - has mounts, endpoints, data_calls arrays
                    if props.get("mounts").is_some()
                        && props.get("endpoints").is_some()
                        && props.get("data_calls").is_some()
                    {
                        return generate_mock_file_analysis_response(prompt);
                    }
                    // Check for framework guidance schema - has mount_patterns, endpoint_patterns, etc.
                    if props.get("mount_patterns").is_some()
                        && props.get("endpoint_patterns").is_some()
                        && props.get("triage_hints").is_some()
                    {
                        return generate_mock_framework_guidance_response(prompt);
                    }
                    // Check for extraction_config_schema - has a rules array.
                    // An empty rule set is a valid config (no unwrapping).
                    if props.get("rules").is_some() {
                        return r#"{"rules": []}"#.to_string();
                    }
                    // Check for pattern_list_schema - has patterns, descriptions, frameworks arrays
                    if props.get("patterns").is_some()
                        && props.get("descriptions").is_some()
                        && props.get("frameworks").is_some()
                    {
                        return generate_mock_pattern_list_response();
                    }
                    // Check for general_guidance_schema - has triage_hints and parsing_notes
                    if props.get("triage_hints").is_some()
                        && props.get("parsing_notes").is_some()
                        && props.get("mount_patterns").is_none()
                    {
                        return generate_mock_general_guidance_response();
                    }
                }
                // Framework detection or other object schema
                r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#.to_string()
            } else {
                // Framework detection or other object schema
                r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#.to_string()
            }
        }
        None => {
            // No schema - return framework detection format
            r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#
                .to_string()
        }
    }
}

/// Generate mock framework guidance response - returns empty structure for testing
/// The real LLM will provide actual patterns based on detected frameworks
fn generate_mock_framework_guidance_response(_prompt: &str) -> String {
    // In mock mode, return a valid but empty structure
    // The real LLM call will populate this with framework-specific patterns
    r#"{"mount_patterns":[],"endpoint_patterns":[],"middleware_patterns":[],"data_fetching_patterns":[],"triage_hints":"Mock mode - no guidance generated","parsing_notes":"Mock mode - no parsing notes"}"#.to_string()
}

/// Generate mock pattern list response for FrameworkGuidanceAgent pattern fetching
/// Returns basic patterns for common frameworks to enable testing
fn generate_mock_pattern_list_response() -> String {
    r#"{"patterns":["app.get('/path', handler)","app.post('/path', handler)","router.get('/path', handler)","app.use('/path', router)","fetch(url)","axios.get(url)"],"descriptions":["GET endpoint","POST endpoint","Router GET endpoint","Mount router","Fetch call","Axios GET"],"frameworks":["express","express","express","express","fetch","axios"]}"#.to_string()
}

/// Generate mock general guidance response for FrameworkGuidanceAgent
/// Returns empty triage hints and parsing notes
fn generate_mock_general_guidance_response() -> String {
    r#"{"triage_hints":"Mock mode - no triage hints","parsing_notes":"Mock mode - no parsing notes"}"#.to_string()
}

/// Generate mock file analysis response for FileAnalyzerAgent
/// Parses the file content from the prompt and extracts mock findings
fn generate_mock_file_analysis_response(prompt: &str) -> String {
    // Extract file path from prompt (format: "Path: path/to/file.ts")
    let file_path = prompt
        .lines()
        .find(|line| line.contains("Path:"))
        .and_then(|line| line.split("Path:").nth(1))
        .map(|s| s.trim().trim_end_matches(')'))
        .unwrap_or("unknown.ts");

    let mut candidate_by_line: HashMap<i32, (String, Option<u32>, Option<u32>)> = HashMap::new();
    let mut candidate_snippets: Vec<(String, Option<u32>, Option<u32>, String)> = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some(candidate_id) = value.get("candidate_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(line_number) = value.get("line_number").and_then(|v| v.as_i64()) else {
            continue;
        };
        let span_start = value
            .get("span_start")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let span_end = value
            .get("span_end")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        if let Some(code_snippet) = value.get("code_snippet").and_then(|v| v.as_str()) {
            candidate_snippets.push((
                candidate_id.to_string(),
                span_start,
                span_end,
                code_snippet.to_string(),
            ));
        }
        candidate_by_line.insert(
            line_number as i32,
            (candidate_id.to_string(), span_start, span_end),
        );
    }

    // Look for common patterns in the file content to generate mock results
    let mut mounts = Vec::new();
    let mut endpoints = Vec::new();
    let mut data_calls = Vec::new();

    // Find where the actual FILE CONTENT section starts (after "### FILE CONTENT")
    // This avoids detecting patterns from the framework guidance examples
    let file_content_start = prompt
        .find("### FILE CONTENT")
        .or_else(|| prompt.find("FILE CONTENT"))
        .unwrap_or(0);

    let content_section = &prompt[file_content_start..];
    let content_to_analyze = if let Some(fence_start) = content_section.find("```") {
        let after_fence = &content_section[fence_start + 3..];
        if let Some(fence_end) = after_fence.find("```") {
            &after_fence[..fence_end]
        } else {
            after_fence
        }
    } else {
        content_section
    };

    let resolve_candidate = |line_number: i32, line_text: &str| {
        if let Some(entry) = candidate_by_line.get(&line_number) {
            return entry.clone();
        }
        let trimmed_line = line_text.trim();
        if !trimmed_line.is_empty()
            && let Some(entry) = candidate_snippets.iter().find(|(_, _, _, snippet)| {
                snippet.contains(trimmed_line) || trimmed_line.contains(snippet)
            })
        {
            return (entry.0.clone(), entry.1, entry.2);
        }
        (format!("line:{}", line_number), None, None)
    };

    // Simple pattern matching on prompt content for mock generation
    // Only look at lines that are likely actual code (not comments, not in strings)
    for (line_num, line) in content_to_analyze.lines().enumerate() {
        let line_number = (line_num + 1) as i32;
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.is_empty() {
            continue;
        }

        // Skip lines that are clearly not endpoint definitions
        // (e.g., interface definitions, type annotations, etc.)
        if trimmed.starts_with("interface")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("export type")
        {
            continue;
        }

        // Detect .use() mounts - must have a path string argument
        if (line.contains("app.use(")
            || line.contains("Router.use(")
            || line.contains("router.use(")
            || line.contains("apiRouter.use("))
            && (line.contains("\"/") || line.contains("'/"))
        {
            // Extract parent node name
            let parent = if line.contains("app.use") {
                "app"
            } else if line.contains("apiRouter.use") {
                "apiRouter"
            } else if line.contains("v1Router.use") {
                "v1Router"
            } else {
                "router"
            };

            // Try to extract the mount path
            let mount_path = extract_path_from_line(line).unwrap_or("/".to_string());

            mounts.push(serde_json::json!({
                "line_number": line_number,
                "parent_node": parent,
                "child_node": "childRouter",
                "mount_path": mount_path,
                "import_source": null,
                "pattern_matched": ".use("
            }));
        }

        // Detect endpoint patterns - must be on app/router object and have a path string
        // More specific patterns to avoid false positives
        let is_endpoint_call = (line.contains("app.get(")
            || line.contains("router.get(")
            || line.contains("v1Router.get(")
            || line.contains("apiRouter.get(")
            || line.contains("adminRouter.get("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_endpoint_call {
            let owner = extract_owner_from_line(line, "get");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "GET",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".get(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        let is_post_call = (line.contains("app.post(")
            || line.contains("router.post(")
            || line.contains("v1Router.post(")
            || line.contains("apiRouter.post(")
            || line.contains("adminRouter.post("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_post_call {
            let owner = extract_owner_from_line(line, "post");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "POST",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".post(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect DELETE endpoints
        let is_delete_call = (line.contains("app.delete(")
            || line.contains("router.delete(")
            || line.contains("v1Router.delete(")
            || line.contains("apiRouter.delete(")
            || line.contains("adminRouter.delete("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_delete_call {
            let owner = extract_owner_from_line(line, "delete");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "DELETE",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".delete(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect PUT endpoints
        let is_put_call = (line.contains("app.put(")
            || line.contains("router.put(")
            || line.contains("v1Router.put(")
            || line.contains("apiRouter.put(")
            || line.contains("adminRouter.put("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_put_call {
            let owner = extract_owner_from_line(line, "put");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "PUT",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".put(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect fetch calls - but not response.json() or similar
        if line.contains("fetch(") && !line.contains("response") && !line.contains("res.") {
            let target =
                extract_url_from_line(line).unwrap_or("https://api.example.com".to_string());
            let method = if line.contains("method:") && line.contains("POST") {
                "POST"
            } else {
                "GET"
            };
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            data_calls.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "target": target,
                "method": method,
                "pattern_matched": "fetch(",
                "call_expression_text": null,
                "call_expression_line": null,
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect axios calls
        if line.contains("axios.get")
            || line.contains("axios.post")
            || line.contains("axios.put")
            || line.contains("axios.delete")
        {
            let method = if line.contains("axios.post") {
                "POST"
            } else if line.contains("axios.put") {
                "PUT"
            } else if line.contains("axios.delete") {
                "DELETE"
            } else {
                "GET"
            };
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            data_calls.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "target": "https://api.example.com",
                "method": method,
                "pattern_matched": "axios.",
                "call_expression_text": null,
                "call_expression_line": null,
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }
    }

    // Log mock generation for debugging
    debug!(
        "Mock file analysis for {}: {} mounts, {} endpoints, {} data_calls",
        file_path,
        mounts.len(),
        endpoints.len(),
        data_calls.len()
    );

    serde_json::json!({
        "mounts": mounts,
        "endpoints": endpoints,
        "data_calls": data_calls
    })
    .to_string()
}

/// Helper to extract path from a line like: app.get("/users", handler)
fn extract_path_from_line(line: &str) -> Option<String> {
    // Try double quotes first
    if let Some(start) = line.find("\"")
        && let Some(end) = line[start + 1..].find("\"")
    {
        let path = &line[start + 1..start + 1 + end];
        if path.starts_with('/') {
            return Some(path.to_string());
        }
    }
    // Try single quotes
    if let Some(start) = line.find("'")
        && let Some(end) = line[start + 1..].find("'")
    {
        let path = &line[start + 1..start + 1 + end];
        if path.starts_with('/') {
            return Some(path.to_string());
        }
    }
    None
}

/// Helper to extract owner from a line like: router.get("/path", ...)
fn extract_owner_from_line(line: &str, method: &str) -> String {
    let pattern = format!(".{}(", method);
    if let Some(idx) = line.find(&pattern) {
        let before = &line[..idx];
        // Get the last word before the dot
        let words: Vec<&str> = before.split_whitespace().collect();
        if let Some(last) = words.last() {
            // Clean up any remaining characters
            let cleaned = last.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !cleaned.is_empty() {
                return cleaned.to_string();
            }
        }
    }
    "router".to_string()
}

/// Helper to extract URL from fetch call
fn extract_url_from_line(line: &str) -> Option<String> {
    // Handle template literals and string literals
    if let Some(path) = extract_path_from_line(line) {
        return Some(path);
    }
    // Handle backtick template literals
    if let Some(start) = line.find('`')
        && let Some(end) = line[start + 1..].find('`')
    {
        return Some(line[start + 1..start + 1 + end].to_string());
    }
    None
}

/// Generate mock triage responses by extracting locations from prompt
fn generate_mock_triage_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);

    let triage_results: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("");

            let args = cs.get("args").and_then(|a| a.as_array());
            let arg_count = cs
                .get("arg_count")
                .and_then(|c| c.as_u64())
                .map(|c| c as usize)
                .or_else(|| args.map(|a| a.len()))
                .unwrap_or(0);

            let has_correlated_call = cs
                .get("correlated_call")
                .map(|v| !v.is_null())
                .unwrap_or(false);

            let classification = if matches!(
                callee_property,
                "json" | "text" | "blob" | "arrayBuffer" | "formData"
            ) {
                if has_correlated_call {
                    "DataFetchingCall"
                } else {
                    "Irrelevant"
                }
            } else if callee_object == "global" && callee_property == "fetch" {
                "DataFetchingCall"
            } else if matches!(callee_property, "get" | "post" | "put" | "delete" | "patch") {
                if callee_object == "axios" || callee_object == "request" || callee_object == "http"
                {
                    "DataFetchingCall"
                } else {
                    "HttpEndpoint"
                }
            } else if callee_property == "use" {
                if arg_count >= 2 {
                    let first_is_string = cs
                        .get("first_arg_type")
                        .and_then(|t| t.as_str())
                        .map(|t| t == "StringLiteral")
                        .or_else(|| {
                            args.and_then(|a| a.first())
                                .and_then(|arg| arg.get("arg_type"))
                                .and_then(|t| t.as_str())
                                .map(|t| t == "StringLiteral")
                        })
                        .unwrap_or(false);

                    // For LeanCallSite we don't have second arg info, so we assume RouterMount
                    // if first arg is string and arg_count >= 2.
                    // For full CallSite we check second arg is Identifier.
                    let second_is_id = args
                        .and_then(|a| a.get(1))
                        .and_then(|arg| arg.get("arg_type"))
                        .and_then(|t| t.as_str())
                        == Some("Identifier");

                    if first_is_string && (args.is_none() || second_is_id) {
                        "RouterMount"
                    } else {
                        "Middleware"
                    }
                } else {
                    "Middleware"
                }
            } else if arg_count >= 2 {
                let first_is_id = args
                    .and_then(|a| a.first())
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str())
                    == Some("Identifier");

                let second_is_object = args
                    .and_then(|a| a.get(1))
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str())
                    == Some("ObjectLiteral");

                if first_is_id && second_is_object {
                    let context_slice = cs
                        .get("context_slice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if extract_path_prefix_from_context_slice(context_slice).is_some() {
                        "RouterMount"
                    } else {
                        "Irrelevant"
                    }
                } else {
                    "Irrelevant"
                }
            } else {
                "Irrelevant"
            };

            serde_json::json!({
                "location": location,
                "classification": classification,
                "confidence": 0.9
            })
        })
        .collect();

    serde_json::to_string(&triage_results).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock endpoint responses
fn generate_mock_endpoint_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let endpoints: Vec<serde_json::Value> = call_sites
        .iter()
        .filter_map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let raw_path = cs
                .get("args")
                .and_then(|args| args.as_array())
                .and_then(|arr| arr.first())
                .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                .and_then(|v| v.as_str())
                .unwrap_or("/");

            let context_slice = cs
                .get("context_slice")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let inferred_prefix = if !context_slice.is_empty()
                && context_slice.contains(callee_object)
                && context_slice.contains("prefix")
            {
                extract_path_prefix_from_context_slice(context_slice)
            } else {
                None
            };

            let path = if let Some(prefix) = inferred_prefix {
                join_path_prefix(&prefix, raw_path)
            } else {
                raw_path.to_string()
            };

            if matches!(callee_property, "get" | "post" | "put" | "delete" | "patch") {
                Some(serde_json::json!({
                    "method": callee_property.to_uppercase(),
                    "path": path,
                    "handler": "handler",
                    "node_name": callee_object,
                    "location": location,
                    "confidence": 0.9,
                    "reasoning": "Mock endpoint extraction"
                }))
            } else {
                None
            }
        })
        .collect();

    serde_json::to_string(&endpoints).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock consumer (data fetching) responses
fn generate_mock_consumer_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);

    let consumers: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let correlated = cs.get("correlated_call");
            let correlated_callee = correlated
                .and_then(|c| c.get("callee"))
                .and_then(|v| v.as_str());
            let correlated_url = correlated
                .and_then(|c| c.get("url"))
                .and_then(|v| v.as_str());
            let correlated_method = correlated
                .and_then(|c| c.get("method"))
                .and_then(|v| v.as_str());

            let args = cs.get("args").and_then(|a| a.as_array());
            let arg0_value = args
                .and_then(|a| a.first())
                .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                .and_then(|v| v.as_str());

            let url: Option<String> = correlated_url
                .or(arg0_value)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());

            let method: Option<String> =
                correlated_method
                    .map(|s| s.to_string())
                    .or_else(|| match callee_property {
                        "get" | "post" | "put" | "delete" | "patch" => {
                            Some(callee_property.to_uppercase())
                        }
                        _ => None,
                    });

            let is_decode_call = matches!(
                callee_property,
                "json" | "text" | "blob" | "arrayBuffer" | "formData"
            ) && args.map(|a| a.is_empty()).unwrap_or(false);

            let library = if is_decode_call {
                correlated_callee
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "response_parsing".to_string())
            } else if callee_object == "global" {
                callee_property.to_string()
            } else if let Some(callee) = correlated_callee {
                callee.to_string()
            } else {
                callee_object.to_string()
            };

            serde_json::json!({
                "library": library,
                "url": url,
                "method": method,
                "location": location,
                "confidence": 0.8,
                "reasoning": "Mock data fetching call"
            })
        })
        .collect();

    serde_json::to_string(&consumers).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock mount relationship responses
fn generate_mock_mount_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let mounts: Vec<serde_json::Value> = call_sites
        .iter()
        .filter_map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let args = cs.get("args").and_then(|a| a.as_array());

            if args.map(|a| a.len()).unwrap_or(0) >= 2 {
                let first_arg_type = args
                    .and_then(|a| a.first())
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str());

                let second_arg_type = args
                    .and_then(|a| a.get(1))
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str());

                if callee_property == "use"
                    && first_arg_type == Some("StringLiteral")
                    && second_arg_type == Some("Identifier")
                {
                    let path = args
                        .and_then(|a| a.first())
                        .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("/");
                    let child = args
                        .and_then(|a| a.get(1))
                        .and_then(|arg| arg.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("router");

                    return Some(serde_json::json!({
                        "parent_node": callee_object,
                        "child_node": child,
                        "mount_path": path,
                        "location": location,
                        "confidence": 0.9,
                        "reasoning": "Mock mount extraction"
                    }));
                }

                if first_arg_type == Some("Identifier") && second_arg_type == Some("ObjectLiteral")
                {
                    let child = args
                        .and_then(|a| a.first())
                        .and_then(|arg| arg.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("router");

                    let context_slice = cs
                        .get("context_slice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if let Some(prefix) = extract_path_prefix_from_context_slice(context_slice) {
                        return Some(serde_json::json!({
                            "parent_node": callee_object,
                            "child_node": child,
                            "mount_path": prefix,
                            "location": location,
                            "confidence": 0.9,
                            "reasoning": "Mock mount extraction"
                        }));
                    }
                }
            }

            None
        })
        .collect();

    serde_json::to_string(&mounts).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock middleware responses
fn generate_mock_middleware_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let middleware: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            serde_json::json!({
                "middleware_type": "custom",
                "path_prefix": null,
                "handler": callee_property,
                "node_name": callee_object,
                "location": location,
                "confidence": 0.8,
                "reasoning": "Mock middleware"
            })
        })
        .collect();

    serde_json::to_string(&middleware).unwrap_or_else(|_| "[]".to_string())
}

fn extract_path_prefix_from_context_slice(context_slice: &str) -> Option<String> {
    extract_string_literal_after_key(context_slice, "prefix")
        .or_else(|| extract_string_literal_after_key(context_slice, "basePath"))
        .or_else(|| extract_string_literal_after_key(context_slice, "base_path"))
        .or_else(|| extract_string_literal_after_key(context_slice, "pathPrefix"))
        .or_else(|| extract_string_literal_after_key(context_slice, "path_prefix"))
        .filter(|v| v.starts_with('/'))
        .map(|v| v.to_string())
}

fn extract_string_literal_after_key(haystack: &str, key: &str) -> Option<String> {
    let hay = haystack.as_bytes();
    let key_bytes = key.as_bytes();
    let mut i = 0;

    while i + key_bytes.len() <= hay.len() {
        if &hay[i..i + key_bytes.len()] == key_bytes {
            let mut j = i + key_bytes.len();

            while j < hay.len() && hay[j].is_ascii_whitespace() {
                j += 1;
            }

            if j >= hay.len() || (hay[j] != b':' && hay[j] != b'=') {
                i += key_bytes.len();
                continue;
            }

            j += 1;
            while j < hay.len() && hay[j].is_ascii_whitespace() {
                j += 1;
            }

            if j >= hay.len() || (hay[j] != b'\'' && hay[j] != b'"') {
                i += key_bytes.len();
                continue;
            }

            let quote = hay[j];
            j += 1;
            let start_val = j;

            while j < hay.len() && hay[j] != quote {
                j += 1;
            }

            if j >= hay.len() {
                return None;
            }

            let value = String::from_utf8_lossy(&hay[start_val..j]).to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }

        i += 1;
    }

    None
}

fn join_path_prefix(prefix: &str, path: &str) -> String {
    let normalized_prefix = prefix.trim_end_matches('/');
    let normalized_path = path.trim_start_matches('/');

    if normalized_prefix.is_empty() {
        format!("/{}", normalized_path)
    } else if normalized_path.is_empty() {
        normalized_prefix.to_string()
    } else {
        format!("{}/{}", normalized_prefix, normalized_path)
    }
}

/// Helper function to extract call sites from prompt JSON
fn extract_call_sites_from_prompt(prompt: &str) -> Vec<serde_json::Value> {
    // Try multiple search patterns for compact and pretty-printed JSON
    let patterns = [
        "[{\"callee_object\"",           // Compact JSON
        "[\n  {\n    \"callee_object\"", // Pretty-printed JSON
        "[\n  {\n   \"callee_object\"",  // Alternative indentation
    ];

    for pattern in &patterns {
        if let Some(start) = prompt.find(pattern) {
            // Find matching closing bracket
            if let Some(end_offset) = find_matching_bracket(&prompt[start..]) {
                let json_str = &prompt[start..start + end_offset];
                if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                    return parsed;
                }
            }
        }
    }

    // Fallback: iterate through all JSON arrays to find one that looks like call sites
    // This handles cases where LeanCallSite serialization might differ slightly
    // and avoids picking up other arrays (like frameworks list)
    let mut current_pos = 0;
    while let Some(start) = prompt[current_pos..].find('[') {
        let abs_start = current_pos + start;
        if let Some(end_offset) = find_matching_bracket(&prompt[abs_start..]) {
            let json_str = &prompt[abs_start..abs_start + end_offset];
            if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(json_str)
                && !parsed.is_empty()
                && parsed[0].get("callee_object").is_some()
                && parsed[0].get("location").is_some()
            {
                return parsed;
            }
        }
        current_pos = abs_start + 1;
    }

    vec![]
}

/// Find the matching closing bracket for a JSON array
fn find_matching_bracket(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serial_test::serial;

    /// The terminal hears about retries on the first one and then once per
    /// gap, however many attempts happen in between (carrick#1103).
    #[test]
    fn the_retry_line_is_due_first_and_then_once_per_gap() {
        let start = Instant::now();
        assert!(retry_announcement_due(None, start));
        assert!(!retry_announcement_due(
            Some(start),
            start + Duration::from_secs(5)
        ));
        assert!(retry_announcement_due(
            Some(start),
            start + RETRY_ANNOUNCE_GAP
        ));
    }

    fn err_with_code(code: &str) -> AgentError {
        AgentError {
            code: code.to_string(),
            message: "boom".to_string(),
            retriable: true,
            details: None,
        }
    }

    /// The lease wait as the cloud sends it since carrick#1131.
    const IN_FLIGHT_409: &str = r#"{"success":false,"error":{"code":"analysis_in_flight","message":"still being analysed","retriable":true,"details":{"requestId":"r1","reason":"analysis_in_flight"}}}"#;
    /// The lease wait as a cloud deployed before carrick#1131 sends it.
    const IN_FLIGHT_503: &str = r#"{"success":false,"error":{"code":"model_error","message":"still being analysed","retriable":true,"details":{"requestId":"r1","reason":"analysis_in_flight"}}}"#;
    /// A real capacity refusal: the model's own retries ran out.
    const MODEL_BUSY_503: &str = r#"{"success":false,"error":{"code":"model_error","message":"model busy","retriable":true,"details":{"requestId":"r2"}}}"#;

    fn envelope_error(body: &str) -> AgentError {
        serde_json::from_str::<AgentResponse>(body)
            .unwrap()
            .error
            .unwrap()
    }

    /// `details` is read off every envelope, and an envelope without it (or
    /// with one of a shape nobody expected) still parses.
    #[test]
    fn an_error_envelope_keeps_its_details() {
        let err = envelope_error(IN_FLIGHT_409);
        assert_eq!(err.reason(), Some("analysis_in_flight"));
        assert_eq!(
            err.details.as_ref().and_then(|d| d.get("requestId")),
            Some(&serde_json::json!("r1"))
        );

        let bare = envelope_error(
            r#"{"success":false,"error":{"code":"internal_error","message":"x","retriable":false}}"#,
        );
        assert!(bare.details.is_none());
        assert_eq!(bare.reason(), None);

        let odd = envelope_error(
            r#"{"success":false,"error":{"code":"x","message":"x","retriable":false,"details":{"reason":7}}}"#,
        );
        assert_eq!(odd.reason(), None);
        let scalar = envelope_error(
            r#"{"success":false,"error":{"code":"x","message":"x","retriable":false,"details":"text"}}"#,
        );
        assert_eq!(scalar.reason(), None);
    }

    /// Both wire shapes of the lease wait are recognised, and nothing else is:
    /// a plain `model_error` is a capacity refusal, and another reason on
    /// another code says nothing about a lease.
    #[test]
    fn the_lease_wait_is_recognised_in_both_wire_shapes_and_nothing_else_is() {
        assert!(is_analysis_in_flight(&envelope_error(IN_FLIGHT_409)));
        assert!(is_analysis_in_flight(&envelope_error(IN_FLIGHT_503)));
        assert!(is_analysis_in_flight(&err_with_code(
            ANALYSIS_IN_FLIGHT_CODE
        )));
        assert!(!is_analysis_in_flight(&envelope_error(MODEL_BUSY_503)));
        assert!(!is_analysis_in_flight(&envelope_error(
            r#"{"success":false,"error":{"code":"llm_disabled","message":"x","retriable":false,"details":{"reason":"daily_cap"}}}"#,
        )));
    }

    #[test]
    fn a_lease_wait_sleeps_for_the_hint_or_a_short_default_never_the_backoff() {
        let hint = Some(Duration::from_secs(2));
        assert_eq!(
            in_flight_wait(0, hint, RETRY_MAX_DELAY),
            Duration::from_secs(2)
        );
        assert_eq!(
            in_flight_wait(u32::MAX, hint, RETRY_MAX_DELAY),
            Duration::from_secs(3)
        );
        assert_eq!(
            in_flight_wait(0, None, RETRY_MAX_DELAY),
            IN_FLIGHT_DEFAULT_WAIT
        );
        // The old cloud's 10 s floor is honoured as sent.
        assert_eq!(
            in_flight_wait(0, Some(Duration::from_secs(10)), RETRY_MAX_DELAY),
            Duration::from_secs(10)
        );
        // Capped at the policy's ceiling before the jitter, like any hint.
        assert_eq!(
            in_flight_wait(0, Some(RETRY_AFTER_CAP), RETRY_MAX_DELAY),
            RETRY_MAX_DELAY
        );
    }

    /// File analysis and intents run side by side on separately constructed
    /// services (carrick#1065); the cap on calls in flight holds only if every
    /// instance draws on the same permits.
    #[test]
    fn every_agent_service_shares_one_semaphore() {
        let detection = AgentService::new();
        let intents = AgentService::new();
        assert!(Arc::ptr_eq(&detection.semaphore, &intents.semaphore));
        // A busy model is busy for every service that asks it, so the
        // adaptive limits are shared the same way.
        assert!(Arc::ptr_eq(&detection.limits, &intents.limits));
        // The gateway throttle is one rate for every route.
        assert!(Arc::ptr_eq(&detection.pacer, &intents.pacer));
    }

    #[test]
    fn a_busy_model_and_a_gateway_throttle_are_told_apart() {
        // Envelope: the model's own capacity.
        assert!(is_model_busy(503, None));
        assert!(is_model_busy(500, Some(Duration::from_secs(10))));
        assert!(!is_model_busy(500, None));
        assert!(!is_model_busy(502, None));
        // No envelope: only a 429 is the gateway's rate throttle. A 502/503/
        // 504 may be a lambda timeout and says nothing about rate.
        assert!(is_gateway_throttle(429));
        assert!(!is_gateway_throttle(503));
        assert!(!is_gateway_throttle(504));
    }

    fn service_with(permits: usize, route_max: usize) -> AgentService {
        AgentService {
            client: Client::builder().no_proxy().build().unwrap(),
            semaphore: Arc::new(Semaphore::new(permits)),
            limits: Arc::new(RouteLimits::new(route_max)),
            pacer: Arc::new(RatePacer::new()),
            retry: RetryPolicy::STANDARD,
        }
    }

    /// A gateway 429 is refused before any lambda runs, so the model was never
    /// asked: the re-send must still say attempt 1, or the lambda would give
    /// it a single model try instead of its full chain.
    #[tokio::test]
    async fn a_gateway_throttle_does_not_advance_the_attempt_the_lambda_sees() {
        let (api_base, server) = stub_server(vec![
            (429, r#"{"message":"Too Many Requests"}"#.to_string()),
            (200, r#"{"success":true,"text":"analysed"}"#.to_string()),
        ]);
        let service = service_with(4, 4);
        let result = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/generate-intent",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        let requests = server.join().unwrap();
        for request in &requests {
            assert_eq!(
                header_of(request, "x-carrick-attempt").as_deref(),
                Some("1")
            );
        }
        assert!(service.pacer.rate().is_some(), "the throttle did not pace");
        assert_eq!(
            service.limits.for_route("/generate-intent").limit(),
            4,
            "a gateway throttle cut the route's concurrency"
        );
    }

    /// The two guidance fields are key material for the cloud's analysis
    /// cache, and they are OMITTED when absent — the request bytes an older
    /// cloud parses are then exactly the ones it parsed before
    /// (carrick-cloud#871).
    #[test]
    fn the_analyze_request_carries_the_guidance_split_only_when_there_is_one() {
        let bare = LambdaRequest {
            user_message: "m".to_string(),
            response_schema: None,
            guidance_key: None,
            guidance_prefix_bytes: None,
        };
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"user_message":"m"}"#
        );

        let split = LambdaRequest {
            user_message: "m".to_string(),
            response_schema: None,
            guidance_key: Some("abc".to_string()),
            guidance_prefix_bytes: Some(12),
        };
        assert_eq!(
            serde_json::to_string(&split).unwrap(),
            r#"{"user_message":"m","guidance_key":"abc","guidance_prefix_bytes":12}"#
        );
    }

    /// `guidance_key` rides beside `text` on the framework-guidance envelope
    /// and is absent from every other lambda's, so its reader must treat a
    /// missing field as "no split available" rather than as a bad response.
    #[test]
    fn the_envelope_reads_a_guidance_key_when_one_is_there_and_copes_when_it_is_not() {
        let with: AgentResponse =
            serde_json::from_str(r#"{"success":true,"text":"t","guidance_key":"abc"}"#).unwrap();
        assert_eq!(with.guidance_key.as_deref(), Some("abc"));

        let without: AgentResponse =
            serde_json::from_str(r#"{"success":true,"text":"t"}"#).unwrap();
        assert_eq!(without.guidance_key, None);

        let null: AgentResponse =
            serde_json::from_str(r#"{"success":true,"text":"t","guidance_key":null}"#).unwrap();
        assert_eq!(null.guidance_key, None);
    }

    #[test]
    fn quota_error_classified_by_code() {
        // Only the `rate_limited` code trips the breaker; transient overloads
        // keep their normal backoff path.
        assert!(is_quota_error(&err_with_code("rate_limited")));
        assert!(!is_quota_error(&err_with_code("overloaded")));
        assert!(!is_quota_error(&err_with_code("model_error")));
    }

    #[test]
    #[serial]
    fn breaker_trips_and_is_idempotent() {
        // Process-global state — reset around the assertions so neither this
        // run nor a sibling test leaks a tripped breaker.
        RATE_LIMITED.store(false, Ordering::Relaxed);
        assert!(!rate_limit_tripped());

        trip_rate_limit();
        assert!(rate_limit_tripped());
        trip_rate_limit();
        assert!(rate_limit_tripped());

        RATE_LIMITED.store(false, Ordering::Relaxed);
        assert!(!rate_limit_tripped());
    }

    #[test]
    fn abort_error_names_the_quota() {
        // The message must read as a backend capacity limit, not a code fault.
        let msg = rate_limit_abort_error().to_string().to_lowercase();
        assert!(msg.contains("quota"));
    }

    #[test]
    fn transient_errors_retry_and_permanent_ones_do_not() {
        // The cloud's own `retriable` flag decides — the scanner never
        // re-derives the class from message text. The 429-wrapped 503 the
        // backend raises under Vertex pressure (#460) arrives as
        // `model_error, retriable: true` and must consume a backoff attempt.
        let transient = AgentCallError::transient("model_error", "Gemini overloaded".to_string());
        assert!(should_retry(&transient, 1, MAX_RETRIES));
        assert!(should_retry(&transient, MAX_RETRIES - 1, MAX_RETRIES));
        // ...but only while attempts remain.
        assert!(!should_retry(&transient, MAX_RETRIES, MAX_RETRIES));

        // A permanent failure is never retried, at any attempt.
        let permanent = AgentCallError::permanent("internal_error", "boom".to_string());
        assert!(!should_retry(&permanent, 1, MAX_RETRIES));

        // A quota abort is not a per-call failure: the breaker is open, so
        // retrying only burns more of an exhausted budget.
        assert!(rate_limit_abort_error().is_quota_abort());
        assert!(!should_retry(&rate_limit_abort_error(), 1, MAX_RETRIES));
        assert!(!transient.is_quota_abort());
    }

    #[test]
    fn backoff_is_jittered_exponential_under_a_cap() {
        // Zero jitter is the floor (half the exponential), max jitter the
        // ceiling (the full exponential) — so every waker lands somewhere in
        // the back half of its window instead of all on the same instant.
        for attempt in 1..=MAX_RETRIES {
            let low = backoff_delay(attempt, 0);
            let high = backoff_delay(attempt, u32::MAX);
            assert!(low <= high, "attempt {attempt}: jitter inverted the range");
            assert!(
                high <= RETRY_MAX_DELAY,
                "attempt {attempt}: {high:?} exceeded the {RETRY_MAX_DELAY:?} cap"
            );
            assert!(low >= RETRY_BASE_DELAY / 2);
        }

        // Unjittered schedule: 2, 4, 8, 16, 32, 64 seconds, then held at the cap.
        assert_eq!(backoff_delay(1, u32::MAX), Duration::from_secs(2));
        assert_eq!(backoff_delay(2, u32::MAX), Duration::from_secs(4));
        assert_eq!(backoff_delay(6, u32::MAX), RETRY_MAX_DELAY);
        assert_eq!(backoff_delay(7, u32::MAX), RETRY_MAX_DELAY);
        // Doubling is real, not an artefact of the cap.
        assert!(backoff_delay(3, 0) > backoff_delay(2, 0));
        // A large attempt number cannot overflow into a tiny (or huge) sleep.
        assert_eq!(backoff_delay(64, u32::MAX), RETRY_MAX_DELAY);
    }

    #[test]
    fn retry_after_reads_delta_seconds_and_nothing_else() {
        assert_eq!(parse_retry_after(Some("10")), Some(Duration::from_secs(10)));
        assert_eq!(parse_retry_after(Some(" 3 ")), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(None), None);
        assert_eq!(parse_retry_after(Some("")), None);
        assert_eq!(parse_retry_after(Some("-1")), None);
        assert_eq!(parse_retry_after(Some("2.5")), None);
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            None,
            "the HTTP-date form is not something the cloud sends"
        );
        // A daily-cap reset hint hours away cannot park a worker for hours.
        assert_eq!(parse_retry_after(Some("43200")), Some(RETRY_AFTER_CAP));
    }

    /// Walk a policy's worst case: every attempt answered retriable with the
    /// longest jitter, and return (attempts made, seconds slept).
    fn worst_case(policy: RetryPolicy, retry_after: Option<Duration>) -> (u32, Duration) {
        let mut waited = Duration::ZERO;
        let mut attempt = 1;
        loop {
            let next = retry_wait_within(attempt, u32::MAX, retry_after, policy.max_delay);
            if !policy.permits(attempt, waited, next) {
                return (attempt, waited);
            }
            waited += next;
            attempt += 1;
        }
    }

    /// The per-file policy is the seven attempts it always was; the patient
    /// one, for the calls a whole service depends on, sleeps for up to ten
    /// minutes and never longer, whatever the cloud's hint says.
    #[test]
    fn a_service_level_call_waits_minutes_and_a_file_call_does_not() {
        let (attempts, waited) = worst_case(RetryPolicy::STANDARD, None);
        assert_eq!(attempts, MAX_RETRIES);
        assert!(waited <= Duration::from_secs(130), "{waited:?}");

        let (attempts, waited) = worst_case(RetryPolicy::PATIENT, None);
        assert!(attempts > MAX_RETRIES, "{attempts}");
        assert!(waited > Duration::from_secs(450), "{waited:?}");
        assert!(waited <= Duration::from_secs(600), "{waited:?}");

        // A long Retry-After is honoured up to the policy's own ceiling, and
        // the budget still bounds the total.
        let hint = Some(Duration::from_secs(300));
        assert!(
            retry_wait_within(1, 0, hint, RetryPolicy::PATIENT.max_delay)
                <= PATIENT_RETRY_MAX_DELAY * 3 / 2
        );
        assert!(retry_wait_within(1, 0, hint, RETRY_MAX_DELAY) <= RETRY_MAX_DELAY * 3 / 2);
        let (_, waited) = worst_case(RetryPolicy::PATIENT, hint);
        assert!(waited <= Duration::from_secs(600), "{waited:?}");
    }

    /// A patient call stops sleeping once the run's retry budget is spent
    /// (carrick#1126), and a per-file call never draws on it.
    #[tokio::test(start_paused = true)]
    async fn a_spent_run_budget_ends_patient_retries_and_leaves_file_calls_alone() {
        let _serial = crate::retry_budget::tests::SERIAL.lock().await;
        crate::retry_budget::reset();
        let wait = Duration::from_secs(2);
        assert!(RetryPolicy::PATIENT.permits_in_run(1, Duration::ZERO, wait));

        // Spend all of it, through the same sleep the loop takes.
        RetryPolicy::PATIENT
            .sleep(crate::retry_budget::budget())
            .await;
        assert!(!RetryPolicy::PATIENT.permits_in_run(1, Duration::ZERO, wait));
        assert!(RetryPolicy::STANDARD.permits_in_run(1, Duration::ZERO, wait));

        // A standard sleep charges nothing.
        crate::retry_budget::reset();
        RetryPolicy::STANDARD.sleep(Duration::from_secs(60)).await;
        assert_eq!(crate::retry_budget::spent(), Duration::ZERO);
    }

    #[test]
    fn retry_wait_is_the_backoff_or_the_hint_whichever_is_longer() {
        // No hint: exactly the backoff the loop always used.
        for attempt in 1..=MAX_RETRIES {
            for jitter in [0, u32::MAX / 2, u32::MAX] {
                assert_eq!(
                    retry_wait(attempt, jitter, None),
                    backoff_delay(attempt, jitter)
                );
            }
        }
        // A hint longer than the early backoff wins, jittered into [hint, 1.5 x hint].
        let hint = Some(Duration::from_secs(10));
        assert_eq!(retry_wait(1, 0, hint), Duration::from_secs(10));
        assert_eq!(retry_wait(1, u32::MAX, hint), Duration::from_secs(15));
        // A hint shorter than the backoff never shortens the wait.
        assert_eq!(
            retry_wait(6, u32::MAX, Some(Duration::from_secs(1))),
            RETRY_MAX_DELAY
        );
        // An absurd hint is capped before it is jittered.
        assert!(
            retry_wait(1, u32::MAX, Some(Duration::from_secs(86_400))) <= RETRY_AFTER_CAP * 3 / 2
        );
    }

    /// The loop link end to end (carrick-cloud#875): the first request says it
    /// is attempt 1, the lambda answers a retriable `model_error` with a
    /// `Retry-After`, the scanner waits at least that long, and the re-send
    /// says it is attempt 2 so the lambda asks the model only once.
    #[tokio::test]
    async fn a_model_error_retry_honours_retry_after_and_numbers_the_attempt() {
        let (api_base, server) = stub_server_with_headers(vec![
            (
                503,
                r#"{"success":false,"error":{"code":"model_error","message":"overloaded","retriable":true}}"#
                    .to_string(),
                vec![("Retry-After", "3".to_string())],
            ),
            (
                200,
                r#"{"success":true,"text":"analysed"}"#.to_string(),
                Vec::new(),
            ),
        ]);

        let service = AgentService::new();
        let started = std::time::Instant::now();
        let result = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/generate-intent",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        // The first backoff alone is at most 2 s, so 3 s proves the hint was read.
        assert!(
            started.elapsed() >= Duration::from_secs(3),
            "re-sent after {:?}, before the cloud's Retry-After",
            started.elapsed()
        );

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            header_of(&requests[0], "x-carrick-attempt").as_deref(),
            Some("1")
        );
        assert_eq!(
            header_of(&requests[1], "x-carrick-attempt").as_deref(),
            Some("2")
        );
    }

    /// A call waiting out a retry holds no concurrency slot (carrick#1077).
    /// With ONE slot, call A is refused with `Retry-After: 3` and sleeps; call
    /// B, started after A was refused, must get the slot and finish during
    /// A's sleep instead of queueing behind it. The stub answers connections
    /// in arrival order, so B receiving the second response proves B's request
    /// went out before A's retry.
    #[tokio::test]
    async fn a_call_asleep_between_retries_frees_its_concurrency_slot() {
        let (api_base, server) = stub_server_with_headers(vec![
            (
                503,
                r#"{"success":false,"error":{"code":"model_error","message":"overloaded","retriable":true}}"#
                    .to_string(),
                vec![("Retry-After", "3".to_string())],
            ),
            (
                200,
                r#"{"success":true,"text":"second"}"#.to_string(),
                Vec::new(),
            ),
            (
                200,
                r#"{"success":true,"text":"third"}"#.to_string(),
                Vec::new(),
            ),
        ]);

        let service = service_with(1, 1);
        let semaphore = service.semaphore.clone();
        let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
        let body = serde_json::json!({});

        let call_a = service.post_with_retry(&auth, &api_base, "/analyze-file", &body);
        let call_b = async {
            // Long enough for A's first attempt to be refused, far short of
            // its 3 s wait.
            sleep(Duration::from_millis(500)).await;
            let started = std::time::Instant::now();
            let outcome = service
                .post_with_retry(&auth, &api_base, "/analyze-file", &body)
                .await;
            (outcome, started.elapsed())
        };
        let (a, (b, b_elapsed)) = tokio::join!(call_a, call_b);

        assert_eq!(b.unwrap().text, "second", "B queued behind A's retry sleep");
        assert!(
            b_elapsed < Duration::from_secs(2),
            "B waited {b_elapsed:?} for a slot held by a sleeping call"
        );
        assert_eq!(a.unwrap().text, "third");
        assert_eq!(server.join().unwrap().len(), 3);
        assert_eq!(semaphore.available_permits(), 1);
    }

    /// The other half of the same contract: an attempt does wait for a slot.
    /// With the only permit held elsewhere the request must not go out; once
    /// it is released, it does.
    #[tokio::test]
    async fn an_attempt_waits_for_a_free_concurrency_slot() {
        let (api_base, server) = stub_server(vec![(
            200,
            r#"{"success":true,"text":"analysed"}"#.to_string(),
        )]);
        let service = service_with(1, 1);
        let semaphore = service.semaphore.clone();
        let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
        let body = serde_json::json!({});

        let held = semaphore.clone().acquire_owned().await.unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(300),
            service.post_with_retry(&auth, &api_base, "/analyze-file", &body),
        )
        .await;
        assert!(blocked.is_err(), "sent a request without a free slot");

        drop(held);
        let result = service
            .post_with_retry(&auth, &api_base, "/analyze-file", &body)
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        assert_eq!(server.join().unwrap().len(), 1);
    }

    /// A concurrent HTTP stub: every connection runs on its own task and is
    /// answered by `respond`, given the full request text. The sequential
    /// [`stub_server`] cannot see concurrency or rate, which is what the
    /// limiter and the pacer respond to.
    async fn concurrent_stub<F, Fut>(respond: F) -> String
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = StubResponse> + Send,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let respond = Arc::new(respond);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let respond = Arc::clone(&respond);
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let Ok(n) = stream.read(&mut buf).await else {
                            return;
                        };
                        raw.extend_from_slice(&buf[..n]);
                        if n == 0 {
                            break;
                        }
                        let text = String::from_utf8_lossy(&raw).to_string();
                        let Some(header_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let content_length = header_of(&text[..header_end], "content-length")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                    let (status, body, headers) =
                        respond(String::from_utf8_lossy(&raw).to_string()).await;
                    let extra: String = headers
                        .iter()
                        .map(|(name, value)| format!("{name}: {value}\r\n"))
                        .collect();
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         {extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        });
        format!("http://{}", addr)
    }

    /// What the busy mock lambda saw.
    #[derive(Default)]
    struct BusyLambdaCounts {
        /// Requests above the threshold, answered 503 `model_error`.
        refused: std::sync::atomic::AtomicUsize,
        /// The highest `X-Carrick-Attempt` any request carried.
        highest_attempt: std::sync::atomic::AtomicUsize,
    }

    /// A lambda whose model has room for `threshold` requests at once: each
    /// request it serves takes `hold`, and a request that arrives while
    /// `threshold` are already being served is refused at once with a 503
    /// `model_error` and `Retry-After: 1`, the shape the cloud sends when its
    /// own model retries ran out. The threshold is shared so a test can lift
    /// it mid-run.
    async fn busy_lambda(
        threshold: Arc<std::sync::atomic::AtomicUsize>,
        hold: Duration,
    ) -> (String, Arc<BusyLambdaCounts>) {
        use std::sync::atomic::AtomicUsize;

        let counts = Arc::new(BusyLambdaCounts::default());
        let in_flight = Arc::new(AtomicUsize::new(0));
        let shared = Arc::clone(&counts);
        let base = concurrent_stub(move |request| {
            let counts = Arc::clone(&shared);
            let in_flight = Arc::clone(&in_flight);
            let threshold = Arc::clone(&threshold);
            async move {
                let attempt = header_of(&request, "x-carrick-attempt")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                counts.highest_attempt.fetch_max(attempt, Ordering::SeqCst);

                let serving = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                if serving > threshold.load(Ordering::SeqCst) {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    counts.refused.fetch_add(1, Ordering::SeqCst);
                    return (
                        503,
                        r#"{"success":false,"error":{"code":"model_error","message":"model busy","retriable":true}}"#
                            .to_string(),
                        vec![("Retry-After", "1".to_string())],
                    );
                }
                tokio::time::sleep(hold).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                (
                    200,
                    r#"{"success":true,"text":"analysed"}"#.to_string(),
                    Vec::new(),
                )
            }
        })
        .await;
        (base, counts)
    }

    /// An API Gateway stage throttle in front of instant cached answers: a
    /// token bucket holding `burst` requests and refilling `per_second`. A
    /// request that finds it empty gets the gateway's own 429, whose body is
    /// JSON but not an envelope. `lifted` turns the throttle off mid-test.
    async fn throttling_gateway(
        burst: f64,
        per_second: f64,
        lifted: Arc<AtomicBool>,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::AtomicUsize;

        let throttled = Arc::new(AtomicUsize::new(0));
        let bucket = Arc::new(Mutex::new((burst, std::time::Instant::now())));
        let shared = Arc::clone(&throttled);
        let base = concurrent_stub(move |_request| {
            let throttled = Arc::clone(&shared);
            let bucket = Arc::clone(&bucket);
            let lifted = Arc::clone(&lifted);
            async move {
                let admitted = lifted.load(Ordering::SeqCst) || {
                    let mut bucket = bucket.lock().unwrap();
                    let now = std::time::Instant::now();
                    let refill = now.duration_since(bucket.1).as_secs_f64() * per_second;
                    *bucket = ((bucket.0 + refill).min(burst), now);
                    if bucket.0 >= 1.0 {
                        bucket.0 -= 1.0;
                        true
                    } else {
                        false
                    }
                };
                if admitted {
                    (
                        200,
                        r#"{"success":true,"text":"cached"}"#.to_string(),
                        Vec::new(),
                    )
                } else {
                    throttled.fetch_add(1, Ordering::SeqCst);
                    (
                        429,
                        r#"{"message":"Too Many Requests"}"#.to_string(),
                        Vec::new(),
                    )
                }
            }
        })
        .await;
        (base, throttled)
    }

    /// The gateway half of carrick-cloud#869. Cached answers come back at
    /// once, so the concurrency cap does not bound the rate, and 150 calls at
    /// a cap of 28 run straight into a throttle of 20 burst / 50 a second.
    /// The pacer must set a rate the gateway can take, every call must finish,
    /// and the route's concurrency must be left alone, because the model was
    /// never the problem. Once the throttle lifts, the rate climbs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_gateway_throttle_paces_every_route_and_the_pace_recovers() {
        const CAP: usize = 28;
        const CALLS: usize = 150;
        const RECOVERY_CALLS: usize = 100;
        const PER_SECOND: f64 = 50.0;

        let lifted = Arc::new(AtomicBool::new(false));
        let (api_base, throttled) = throttling_gateway(20.0, PER_SECOND, Arc::clone(&lifted)).await;
        let service = service_with(CAP, CAP);
        let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
        let body = serde_json::json!({});
        let run =
            |n: usize| {
                let service = &service;
                let auth = &auth;
                let body = &body;
                let api_base = &api_base;
                futures::future::join_all((0..n).map(move |_| {
                    service.post_with_retry(auth, api_base, "/generate-intent", body)
                }))
            };

        let started = std::time::Instant::now();
        let busy = run(CALLS).await;
        let busy_elapsed = started.elapsed();
        let failed = busy.iter().filter(|r| r.is_err()).count();
        let throttles = throttled.load(Ordering::SeqCst);
        let paced_at = service.pacer.rate();
        let route_limit = service.limits.for_route("/generate-intent").limit();

        lifted.store(true, Ordering::SeqCst);
        let recovery = run(RECOVERY_CALLS).await;
        let recovered_at = service.pacer.rate();

        eprintln!(
            "gateway phase: {CALLS} calls at cap {CAP} against 20 burst / {PER_SECOND}/s; \
             {throttles} throttled, {failed} failed calls, paced at {paced_at:?}/s, route \
             limit {route_limit}, {busy_elapsed:?}. recovery: {RECOVERY_CALLS} calls, paced at \
             {recovered_at:?}/s"
        );

        assert_eq!(failed, 0, "a call exhausted its retries: {busy:?}");
        let paced_at = paced_at.expect("the throttle never set a pace");
        assert!(
            f64::from(paced_at) <= PER_SECOND * 1.5,
            "paced at {paced_at}/s against a {PER_SECOND}/s throttle"
        );
        assert!(
            throttles <= 60,
            "{throttles} throttles for {CALLS} calls: the pace did not hold the rate down"
        );
        assert_eq!(
            route_limit, CAP,
            "a gateway throttle cut the route's concurrency"
        );
        assert!(recovery.iter().all(|r| r.is_ok()));
        assert!(
            recovered_at.expect("pace vanished") > paced_at,
            "the pace did not climb once the gateway let requests through"
        );
    }

    /// A retry policy with the standard shape and millisecond sleeps, so a
    /// test can walk a whole attempt chain.
    fn quick_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            max_delay: Duration::from_millis(20),
            wait_budget: Duration::from_secs(60),
            run_budgeted: false,
        }
    }

    /// carrick#1131: a lease wait, in either wire shape, is sat out without
    /// spending an attempt. With a policy of two attempts, eight waits then
    /// an answer still succeed, every request says it is attempt 1 because
    /// the model was never asked, and the route's limit never moves.
    #[tokio::test]
    async fn a_lease_wait_costs_no_attempt_and_leaves_the_limit_alone() {
        let mut responses: Vec<StubResponse> = (0..MAX_IN_FLIGHT_WAITS)
            .map(|i| {
                let body = if i % 2 == 0 {
                    IN_FLIGHT_409
                } else {
                    IN_FLIGHT_503
                };
                let status = if i % 2 == 0 { 409 } else { 503 };
                (
                    status,
                    body.to_string(),
                    vec![("Retry-After", "0".to_string())],
                )
            })
            .collect();
        responses.push((
            200,
            r#"{"success":true,"text":"analysed"}"#.to_string(),
            Vec::new(),
        ));
        let (api_base, server) = stub_server_with_headers(responses);

        let service = service_with(4, 4).with_retry_policy(quick_policy(2));
        let limit = service.limits.for_route("/analyze-file");
        let result = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        assert_eq!(limit.limit(), 4, "a lease wait cut the route's limit");

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), MAX_IN_FLIGHT_WAITS as usize + 1);
        for request in &requests {
            assert_eq!(
                header_of(request, "x-carrick-attempt").as_deref(),
                Some("1")
            );
        }
    }

    /// Past [`MAX_IN_FLIGHT_WAITS`], a lease that never clears spends attempts
    /// like any retriable error, so the call still ends. It ends named for
    /// what it waited on, even when the old wire shape carried it, and the
    /// limit still hears nothing.
    #[tokio::test]
    async fn a_lease_that_never_clears_ends_the_call_once_the_waits_and_attempts_are_spent() {
        const ATTEMPTS: u32 = 3;
        let total = MAX_IN_FLIGHT_WAITS + ATTEMPTS;
        let responses: Vec<StubResponse> = (0..total)
            .map(|_| {
                (
                    503,
                    IN_FLIGHT_503.to_string(),
                    vec![("Retry-After", "0".to_string())],
                )
            })
            .collect();
        let (api_base, server) = stub_server_with_headers(responses);

        let service = service_with(4, 4).with_retry_policy(quick_policy(ATTEMPTS));
        let limit = service.limits.for_route("/analyze-file");
        let err = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ANALYSIS_IN_FLIGHT_CODE);
        assert!(err.retriable);
        assert_eq!(limit.limit(), 4, "a lease wait cut the route's limit");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), total as usize);
        assert_eq!(
            header_of(requests.last().unwrap(), "x-carrick-attempt").as_deref(),
            Some(ATTEMPTS.to_string().as_str()),
            "the waits past the cap are the ones that spend attempts"
        );
    }

    /// What a released scanner (0.3.70 and earlier) does with the new reply,
    /// and the rule the cloud relies on for it: a retriable envelope is
    /// retried on `retriable` whatever its status and code, and a 4xx with no
    /// `Retry-After` says nothing about capacity.
    #[tokio::test]
    async fn a_retriable_envelope_with_an_unknown_code_on_a_4xx_is_retried_without_a_cut() {
        let (api_base, server) = stub_server(vec![
            (
                409,
                r#"{"success":false,"error":{"code":"some_future_wait","message":"later","retriable":true}}"#
                    .to_string(),
            ),
            (200, r#"{"success":true,"text":"analysed"}"#.to_string()),
        ]);
        let service = service_with(4, 4).with_retry_policy(quick_policy(2));
        let limit = service.limits.for_route("/analyze-file");
        let result = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        assert_eq!(limit.limit(), 4);
        assert_eq!(server.join().unwrap().len(), 2);
    }

    /// carrick#1131 under load: forty calls start together under a route limit
    /// of sixteen, and the first request of each call is answered with a
    /// lease wait (alternating the two wire shapes, each with a `Retry-After`,
    /// as the scale test saw). The limit must never move. Then the same load
    /// against a model that refuses for capacity, carrying the same
    /// `Retry-After`, must still cut it: the fix narrows what counts as a
    /// refusal and leaves real refusals alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lease_waits_never_cut_the_limit_while_capacity_refusals_still_do() {
        use std::sync::atomic::AtomicUsize;
        const MAX: usize = 16;
        const CALLS: usize = 40;

        async fn run_phase(
            body_for_first: &'static str,
            status_for_first: u16,
        ) -> (usize, usize, usize) {
            let seen = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
            let waits = Arc::new(AtomicUsize::new(0));
            let shared_waits = Arc::clone(&waits);
            let base = concurrent_stub(move |request| {
                let seen = Arc::clone(&seen);
                let waits = Arc::clone(&shared_waits);
                async move {
                    // Each call sends its index in the body; its first request
                    // gets the refusal, every later one the answer.
                    let id = request.rsplit("\r\n\r\n").next().unwrap_or("").to_string();
                    let first = seen.lock().unwrap().insert(id);
                    if first {
                        let n = waits.fetch_add(1, Ordering::SeqCst);
                        let (status, body) = if body_for_first == IN_FLIGHT_409 && n % 2 == 1 {
                            (503, IN_FLIGHT_503)
                        } else {
                            (status_for_first, body_for_first)
                        };
                        return (
                            status,
                            body.to_string(),
                            vec![("Retry-After", "0".to_string())],
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    (
                        200,
                        r#"{"success":true,"text":"analysed"}"#.to_string(),
                        Vec::new(),
                    )
                }
            })
            .await;

            let service = service_with(MAX, MAX).with_retry_policy(quick_policy(4));
            let limit = service.limits.for_route("/analyze-file");
            let lowest = Arc::new(AtomicUsize::new(MAX));
            let sampler = tokio::spawn({
                let limit = Arc::clone(&limit);
                let lowest = Arc::clone(&lowest);
                async move {
                    loop {
                        lowest.fetch_min(limit.limit(), Ordering::SeqCst);
                        sleep(Duration::from_millis(1)).await;
                    }
                }
            });
            let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
            let bodies: Vec<serde_json::Value> = (0..CALLS)
                .map(|i| serde_json::json!({ "call": i }))
                .collect();
            let results = futures::future::join_all(
                bodies
                    .iter()
                    .map(|body| service.post_with_retry(&auth, &base, "/analyze-file", body)),
            )
            .await;
            sampler.abort();
            let lowest = lowest.load(Ordering::SeqCst).min(limit.limit());
            let failed = results.iter().filter(|r| r.is_err()).count();
            (lowest, failed, waits.load(Ordering::SeqCst))
        }

        let (lowest, failed, waits) = run_phase(IN_FLIGHT_409, 409).await;
        eprintln!("lease waits: {waits} replies, lowest limit {lowest} of {MAX}, {failed} failed");
        assert_eq!(waits, CALLS);
        assert_eq!(failed, 0);
        assert_eq!(lowest, MAX, "a lease wait cut the route's limit");

        let (lowest, failed, refusals) = run_phase(MODEL_BUSY_503, 503).await;
        eprintln!(
            "capacity refusals: {refusals} replies, lowest limit {lowest} of {MAX}, {failed} failed"
        );
        assert_eq!(refusals, CALLS);
        assert_eq!(failed, 0);
        assert!(lowest < MAX, "a capacity refusal no longer cuts the limit");
    }

    /// carrick-cloud#869 end to end, against a mock lambda that refuses every
    /// request above four at once. Sixty calls start together under a route
    /// limit of sixteen, which is the case the fixed cap got wrong: each
    /// refusal frees a slot for the next queued call to be refused too.
    ///
    /// The limit must fall below the threshold, every call must finish inside
    /// its attempt budget, and once the model has room again the limit must
    /// climb back to its maximum. The counts are printed for the PR note.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_busy_model_slows_its_route_below_the_threshold_and_recovers() {
        use std::sync::atomic::AtomicUsize;
        const MAX: usize = 16;
        const THRESHOLD: usize = 4;
        const CALLS: usize = 60;
        const RECOVERY_CALLS: usize = 150;

        let threshold = Arc::new(AtomicUsize::new(THRESHOLD));
        let (api_base, counts) =
            busy_lambda(Arc::clone(&threshold), Duration::from_millis(100)).await;
        let service = service_with(MAX, MAX);
        let limit = service.limits.for_route("/analyze-file");

        // The lowest the limit went, sampled while the calls run.
        let lowest = Arc::new(AtomicUsize::new(MAX));
        let sampler = tokio::spawn({
            let limit = Arc::clone(&limit);
            let lowest = Arc::clone(&lowest);
            async move {
                loop {
                    lowest.fetch_min(limit.limit(), Ordering::SeqCst);
                    sleep(Duration::from_millis(5)).await;
                }
            }
        });

        let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
        let body = serde_json::json!({});
        let run = |n: usize| {
            let service = &service;
            let auth = &auth;
            let body = &body;
            let api_base = &api_base;
            futures::future::join_all(
                (0..n).map(move |_| service.post_with_retry(auth, api_base, "/analyze-file", body)),
            )
        };

        let started = std::time::Instant::now();
        let busy = run(CALLS).await;
        let busy_elapsed = started.elapsed();
        let failed = busy.iter().filter(|r| r.is_err()).count();
        let refused = counts.refused.load(Ordering::SeqCst);
        let highest_attempt = counts.highest_attempt.load(Ordering::SeqCst);
        let lowest_limit = lowest.load(Ordering::SeqCst);
        let limit_after_busy = limit.limit();

        threshold.store(usize::MAX, Ordering::SeqCst);
        let recovery = run(RECOVERY_CALLS).await;
        sampler.abort();
        let recovered_limit = limit.limit();

        eprintln!(
            "busy phase: {CALLS} calls, limit {MAX} -> lowest {lowest_limit} (threshold \
             {THRESHOLD}), ended at {limit_after_busy}; {refused} refusals, {failed} failed \
             calls, highest attempt {highest_attempt}, {busy_elapsed:?}. recovery: \
             {RECOVERY_CALLS} calls, limit back to {recovered_limit}"
        );

        assert_eq!(failed, 0, "a call exhausted its retries: {busy:?}");
        assert!(
            lowest_limit < THRESHOLD,
            "the limit never fell below the model's threshold: lowest {lowest_limit}"
        );
        assert!(
            highest_attempt <= 4,
            "a call needed attempt {highest_attempt} of {MAX_RETRIES}"
        );
        assert!(
            refused <= 40,
            "{refused} refusals for {CALLS} calls: the limit did not hold the load off"
        );
        assert!(recovery.iter().all(|r| r.is_ok()));
        assert_eq!(
            recovered_limit, MAX,
            "the limit did not climb back once the model had room"
        );
    }

    /// A re-mint re-sends without the model ever having been asked, so the
    /// re-sent request is still attempt 1 and keeps the lambda's full chain.
    #[tokio::test]
    async fn a_remint_does_not_advance_the_attempt_the_lambda_sees() {
        let (token_url, token_server) = stub_token_endpoint(vec![
            crate::oidc::tests::jwt_with_exp(unix_now() + 3600),
            crate::oidc::tests::jwt_with_exp(unix_now() + 7200),
        ]);
        let provider = OidcProvider::for_test(token_url, "request-token".to_string());
        let (api_base, api_server) = stub_server(vec![
            (401, r#"{"code":"oidc_invalid"}"#.to_string()),
            (200, r#"{"success":true,"text":"analysed"}"#.to_string()),
        ]);

        let service = AgentService::new();
        let result = service
            .post_with_retry(
                &RequestAuth::Oidc(&provider),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");
        token_server.join().unwrap();
        let requests = api_server.join().unwrap();
        assert_eq!(
            header_of(&requests[0], "x-carrick-attempt").as_deref(),
            Some("1")
        );
        assert_eq!(
            header_of(&requests[1], "x-carrick-attempt").as_deref(),
            Some("1")
        );
    }

    /// A single-shot HTTP stub: answers each connection with the next canned
    /// response and records the request it received. Reads the whole request
    /// (headers plus `Content-Length` body) before replying, because reqwest
    /// treats a response that arrives mid-upload as a transport failure.
    pub(crate) fn stub_server(
        responses: Vec<(u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        stub_server_with_headers(
            responses
                .into_iter()
                .map(|(status, body)| (status, body, Vec::new()))
                .collect(),
        )
    }

    /// One canned response: status, body, and extra headers.
    pub(crate) type StubResponse = (u16, String, Vec<(&'static str, String)>);

    /// [`stub_server`], with extra response headers on each canned response.
    pub(crate) fn stub_server_with_headers(
        responses: Vec<StubResponse>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body, headers) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    if n == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(header_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let content_length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&raw).to_string());
                let extra: String = headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect();
                let response = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\n\
                     {}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    extra,
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            seen
        });
        (format!("http://{}", addr), handle)
    }

    /// A stub GitHub token endpoint that hands out each token in turn.
    pub(crate) fn stub_token_endpoint(
        tokens: Vec<String>,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for token in tokens {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf).unwrap();
                let body = serde_json::json!({ "value": token }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        (format!("http://{}/token?api-version=2.0", addr), handle)
    }

    /// The header value the request carried, for asserting which token was sent.
    fn oidc_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("x-carrick-oidc")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default()
    }

    /// A 401 is a rejection whatever the body says; a gateway 5xx is one only
    /// when the body names the code; a 200 never is, so an analysis result
    /// that quotes the code out of scanned source is not mistaken for one.
    #[test]
    fn oidc_rejection_reads_status_and_body() {
        assert!(is_oidc_rejection(401, ""));
        assert!(is_oidc_rejection(
            503,
            r#"{"code":"oidc_invalid","reason":"token expired"}"#
        ));
        assert!(!is_oidc_rejection(503, "upstream connect error"));
        assert!(!is_oidc_rejection(
            200,
            r#"{"success":true,"text":"the file handles oidc_invalid errors"}"#
        ));
    }

    #[test]
    fn body_excerpt_is_short_and_single_line() {
        assert_eq!(body_excerpt("  a\n  b  "), "a b");
        assert_eq!(body_excerpt("   "), "<empty body>");
        let long = "x".repeat(500);
        let excerpt = body_excerpt(&long);
        assert_eq!(excerpt.chars().count(), 203);
        assert!(excerpt.ends_with("..."));
    }

    /// The #461 path end to end: the cloud rejects the token behind a gateway
    /// 503 whose body is not an envelope, which the retry loop used to spend
    /// its whole budget on. Now the rejection is read out of the body, a fresh
    /// token is minted, and the retry carries it.
    #[tokio::test]
    async fn a_rejected_token_is_reminted_and_the_retry_carries_the_new_one() {
        // Distinct expiries so the two tokens are distinguishable on the wire.
        let (token_url, token_server) = stub_token_endpoint(vec![
            crate::oidc::tests::jwt_with_exp(unix_now() + 3600),
            crate::oidc::tests::jwt_with_exp(unix_now() + 7200),
        ]);
        let provider = OidcProvider::for_test(token_url, "request-token".to_string());

        let (api_base, api_server) = stub_server(vec![
            (
                503,
                r#"{"code":"oidc_invalid","reason":"token expired"}"#.to_string(),
            ),
            (200, r#"{"success":true,"text":"analysed"}"#.to_string()),
        ]);

        let service = AgentService::new();
        let result = service
            .post_with_retry(
                &RequestAuth::Oidc(&provider),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");

        let requests = api_server.join().unwrap();
        token_server.join().unwrap();
        assert_eq!(requests.len(), 2, "expected exactly one retry");
        let first = oidc_header(&requests[0]);
        let second = oidc_header(&requests[1]);
        assert!(!first.is_empty(), "first request carried no OIDC header");
        assert_ne!(
            first, second,
            "the retry re-sent the token that was just rejected"
        );
    }

    /// A rejection that survives a fresh mint is not an expiry, so it fails
    /// loudly and permanently instead of burning the retry budget on a
    /// credential the cloud will keep refusing.
    #[tokio::test]
    async fn a_rejection_after_reminting_is_permanent() {
        // Distinct expiries so the two tokens are distinguishable on the wire.
        let (token_url, token_server) = stub_token_endpoint(vec![
            crate::oidc::tests::jwt_with_exp(unix_now() + 3600),
            crate::oidc::tests::jwt_with_exp(unix_now() + 7200),
        ]);
        let provider = OidcProvider::for_test(token_url, "request-token".to_string());

        let (api_base, api_server) = stub_server(vec![
            (401, r#"{"code":"oidc_invalid"}"#.to_string()),
            (401, r#"{"code":"oidc_invalid"}"#.to_string()),
        ]);

        let service = AgentService::new();
        let err = service
            .post_with_retry(
                &RequestAuth::Oidc(&provider),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "oidc_rejected");
        assert!(!err.retriable, "a rejected fresh token is not transient");
        assert_eq!(api_server.join().unwrap().len(), 2);
        token_server.join().unwrap();
    }

    fn header_of(request: &str, name: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    /// The laptop branch of a prompt-lambda call: the credential goes in
    /// `Authorization`, the OIDC header is absent, and the scan slot rides
    /// along so the cloud's money gates can read the repo out of it rather
    /// than out of anything this client asserts (§1.3, C4).
    #[tokio::test]
    async fn a_bearer_call_sends_the_credential_and_the_scan_slot() {
        crate::credentials::set_scan_id("scan_01J");
        let (api_base, server) = stub_server(vec![(
            200,
            r#"{"success":true,"text":"analysed"}"#.to_string(),
        )]);

        let service = AgentService::new();
        let result = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap().text, "analysed");

        let request = &server.join().unwrap()[0];
        assert_eq!(
            header_of(request, "authorization").as_deref(),
            Some("Bearer carrick_sk_live_test")
        );
        assert!(
            header_of(request, "x-carrick-oidc").is_none(),
            "the laptop branch must not send the OIDC header: {request}"
        );
        // The version gate runs before authentication and applies to both
        // credentials, so it is still sent.
        assert_eq!(
            header_of(request, "x-carrick-scanner-version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // Presence, not value: the slot is a process-global and another test
        // in this binary may have set it first. What matters is that a laptop
        // call carries one at all.
        assert!(
            header_of(request, "x-carrick-scan-id").is_some(),
            "a laptop call must carry its scan slot: {request}"
        );
    }

    /// A Bearer credential cannot be re-minted — only a fresh consent replaces
    /// it — so a rejection is a sentence and a stop, not a retry against the
    /// same token (§8.2).
    #[tokio::test]
    async fn a_rejected_bearer_credential_stops_instead_of_reminting() {
        let (api_base, server) = stub_server(vec![(401, r#"{"code":"oidc_invalid"}"#.to_string())]);

        let service = AgentService::new();
        let err = service
            .post_with_retry(
                &RequestAuth::Bearer("carrick_sk_live_test".to_string()),
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "credential_rejected");
        assert!(!err.retriable);
        assert!(err.message.contains("carrick login"), "{}", err.message);
        assert_eq!(
            server.join().unwrap().len(),
            1,
            "there is no second credential to try"
        );
    }

    // ---------------------------------------------------------------
    // ---------------------------------------------------------------------
    // carrick-cloud#869 measurement harness. These print a table and assert
    // nothing, so they are `#[ignore]`d; they are what `QUIET_RESTORE` was
    // chosen from. Run one with
    //   cargo test --lib agent_service::tests::probe_869 -- --ignored --nocapture
    // ---------------------------------------------------------------------

    #[derive(Default)]
    struct DsqCounts {
        /// Requests the stub answered.
        requests: std::sync::atomic::AtomicUsize,
        /// Requests answered 503 `model_error` because every in-lambda try was
        /// refused. What a scan pays for twice: once in wall clock, once
        /// against the caller's daily request bucket.
        exhausted: std::sync::atomic::AtomicUsize,
        /// Individual model tries, the thing a refusal is drawn against.
        tries: std::sync::atomic::AtomicUsize,
    }

    /// A lambda in front of a model on dynamic shared quota: whether a try is
    /// refused is drawn against a probability that depends only on how long
    /// the run has been going, never on how many requests are in flight.
    ///
    /// That is the shape the 2026-09-14 first index measured
    /// (carrick-cloud#869): two windows of about twenty minutes in which 13.4%
    /// of model calls came back exhausted, 0.3% between them, and no relation
    /// to the scan's own model-call rate — its highest minute of the window,
    /// 171 calls, was refused nothing. The lambda's own chain is modelled
    /// too: three tries on `X-Carrick-Attempt` 1, one on later attempts
    /// (carrick-cloud#875).
    async fn dsq_lambda(
        dip: std::ops::Range<Duration>,
        p_dip: f64,
        p_clear: f64,
        hold: Duration,
    ) -> (String, Arc<DsqCounts>) {
        let counts = Arc::new(DsqCounts::default());
        let started = std::time::Instant::now();
        let draws = Arc::new(std::sync::atomic::AtomicU64::new(0x9E37_79B9_7F4A_7C15));
        let shared = Arc::clone(&counts);
        let base = concurrent_stub(move |request| {
            let counts = Arc::clone(&shared);
            let draws = Arc::clone(&draws);
            let dip = dip.clone();
            async move {
                counts.requests.fetch_add(1, Ordering::SeqCst);
                let attempt = header_of(&request, "x-carrick-attempt")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(1);
                let in_lambda_tries = if attempt <= 1 { 3 } else { 1 };
                let p = if dip.contains(&started.elapsed()) {
                    p_dip
                } else {
                    p_clear
                };
                let mut served = false;
                for _ in 0..in_lambda_tries {
                    counts.tries.fetch_add(1, Ordering::SeqCst);
                    if draw(&draws) >= p {
                        served = true;
                        break;
                    }
                }
                if !served {
                    counts.exhausted.fetch_add(1, Ordering::SeqCst);
                    return (
                        503,
                        r#"{"success":false,"error":{"code":"model_error","message":"RESOURCE_EXHAUSTED","retriable":true}}"#
                            .to_string(),
                        vec![("Retry-After", "0".to_string())],
                    );
                }
                tokio::time::sleep(hold).await;
                (
                    200,
                    r#"{"success":true,"text":"analysed"}"#.to_string(),
                    Vec::new(),
                )
            }
        })
        .await;
        (base, counts)
    }

    /// A uniform in `[0, 1)` from a shared counter, xorshift64*: every arm
    /// draws from the same stream, and a run is reproducible.
    fn draw(state: &std::sync::atomic::AtomicU64) -> f64 {
        let mut x = state
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::SeqCst)
            .wrapping_add(0x9E37_79B9);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The probe's backoff, scaled down with everything else: the shape of
    /// the chain is the scanner's, the numbers are a fraction of it.
    fn probe_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: MAX_RETRIES,
            max_delay: Duration::from_millis(200),
            wait_budget: Duration::from_secs(600),
            run_budgeted: false,
        }
    }

    fn service_with_quiet(permits: usize, route_max: usize, quiet: Duration) -> AgentService {
        AgentService {
            client: Client::builder().no_proxy().build().unwrap(),
            semaphore: Arc::new(Semaphore::new(permits)),
            limits: Arc::new(RouteLimits::with_quiet(route_max, quiet)),
            pacer: Arc::new(RatePacer::new()),
            retry: probe_policy(),
        }
    }

    async fn drive(service: &AgentService, api_base: &str, calls: usize) -> (Duration, usize) {
        let auth = RequestAuth::Bearer("carrick_sk_live_test".to_string());
        let body = serde_json::json!({});
        let at = std::time::Instant::now();
        let results = futures::future::join_all(
            (0..calls).map(|_| service.post_with_retry(&auth, api_base, "/generate-intent", &body)),
        )
        .await;
        (at.elapsed(), results.iter().filter(|r| r.is_err()).count())
    }

    /// The quiet spells to compare, as multiples of one call's latency, so a
    /// probe on a fast route and a probe on a slow one can be read together.
    /// [`QUIET_RESTORE`] is 30 s, which is about 20 call latencies on the
    /// intent route (1.5 s) and about 9 on file analysis (3.4 s p50).
    const PROBE_ARMS: [(&str, u32); 5] = [
        ("run at width only (the old rule)", 0),
        ("quiet spell = 20 call latencies", 20),
        ("quiet spell = 9 call latencies", 9),
        ("quiet spell = 4 call latencies", 4),
        ("quiet spell = 0 (no cut kept)", 1),
    ];

    /// The intent route's shape: many short calls.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "probe: prints a table and takes about two minutes"]
    async fn probe_869_dsq_shape_fast_route() {
        let hold = Duration::from_millis(150);
        dsq_arms(1500, hold, &quiet_arms(hold)).await;
    }

    /// The file-analysis shape: fewer, slower calls, where a limit that climbs
    /// back one slot per round of successes at that width is minutes of
    /// climbing (2026-09-15 scale test).
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "probe: prints a table and takes about two minutes"]
    async fn probe_869_dsq_shape_slow_route() {
        let hold = Duration::from_millis(800);
        dsq_arms(450, hold, &quiet_arms(hold)).await;
    }

    fn quiet_arms(hold: Duration) -> Vec<(String, Duration)> {
        PROBE_ARMS
            .iter()
            .map(|(name, latencies)| {
                let quiet = match latencies {
                    0 => Duration::MAX,
                    n => hold * *n - hold,
                };
                ((*name).to_string(), quiet)
            })
            .collect()
    }

    const PROBE_MAX: usize = DEFAULT_CONCURRENCY_LIMIT;

    async fn dsq_arms(calls: usize, hold: Duration, arms: &[(String, Duration)]) {
        const CAP: usize = PROBE_MAX;
        let dip = Duration::from_secs(2)..Duration::from_secs(8);

        println!(
            "arm | wall clock | requests | exhausted 503 | model tries | failed | back to max \
             after the dip | time below max"
        );
        for (name, quiet) in arms {
            let (api_base, counts) = dsq_lambda(dip.clone(), 0.53, 0.02, hold).await;
            let service = service_with_quiet(28, CAP, *quiet);
            let limit = service.limits.for_route("/generate-intent");
            let stop = Arc::new(AtomicBool::new(false));
            let samples = Arc::new(Mutex::new(Vec::<(f64, usize)>::new()));
            let sampler = {
                let limit = Arc::clone(&limit);
                let stop = Arc::clone(&stop);
                let samples = Arc::clone(&samples);
                let at = std::time::Instant::now();
                tokio::spawn(async move {
                    while !stop.load(Ordering::SeqCst) {
                        samples
                            .lock()
                            .unwrap()
                            .push((at.elapsed().as_secs_f64(), limit.limit()));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
            };
            let (wall, failed) = drive(&service, &api_base, calls).await;
            stop.store(true, Ordering::SeqCst);
            let _ = sampler.await;
            let samples = samples.lock().unwrap().clone();
            let back = samples
                .iter()
                .find(|(t, l)| *t >= dip.end.as_secs_f64() && *l == CAP)
                .map(|(t, _)| *t - dip.end.as_secs_f64());
            let below = samples.iter().filter(|(_, l)| *l < CAP).count() as f64
                / samples.len().max(1) as f64;
            println!(
                "{name} | {:.2}s | {} | {} | {} | {} | {} | {:.0}%",
                wall.as_secs_f64(),
                counts.requests.load(Ordering::SeqCst),
                counts.exhausted.load(Ordering::SeqCst),
                counts.tries.load(Ordering::SeqCst),
                failed,
                back.map_or_else(|| "never".to_string(), |s| format!("{s:.2}s")),
                below * 100.0,
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// The capacity shape the cut was built for (carrick#1119's control): the
    /// backend really does have room for a fixed number at once, so this
    /// scan's own rate is what produces the refusals. The quiet spell must
    /// not undo the cut here — and it cannot, because under this shape the
    /// refusals never stop for one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "probe: prints a table and takes about a minute"]
    async fn probe_869_capacity_shape() {
        let hold = Duration::from_millis(150);
        // Room for four at once: the edge is far below half the maximum.
        capacity_arms(4, hold, &quiet_arms(hold)).await;
        // Room for twenty: the edge is inside the top half of the maximum,
        // where a restore has the furthest to overshoot.
        capacity_arms(20, hold, &quiet_arms(hold)).await;
    }

    async fn capacity_arms(threshold: usize, hold: Duration, arms: &[(String, Duration)]) {
        const CALLS: usize = 300;
        println!("--- backend with room for {threshold} at once, maximum {PROBE_MAX} ---");
        println!("arm | wall clock | refused | failed calls");
        for (name, quiet) in arms {
            let threshold = Arc::new(std::sync::atomic::AtomicUsize::new(threshold));
            let (api_base, counts) = busy_lambda(Arc::clone(&threshold), hold).await;
            let service = service_with_quiet(28, PROBE_MAX, *quiet);
            let (wall, failed) = drive(&service, &api_base, CALLS).await;
            println!(
                "{name} | {:.2}s | {} | {}",
                wall.as_secs_f64(),
                counts.refused.load(Ordering::SeqCst),
                failed,
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// A clean run: nothing is refused, so no limit ever leaves its maximum
    /// and the quiet spell has nothing to restore. The arms must match.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "probe: prints a table"]
    async fn probe_869_clean_run() {
        const CAP: usize = 20;
        const CALLS: usize = 1500;
        let hold = Duration::from_millis(150);

        println!("arm | wall clock | requests");
        for (name, quiet) in [
            ("run at width only (the old rule)", Duration::MAX),
            ("quiet spell = 20 call latencies", hold * 19),
        ] {
            let (api_base, counts) = dsq_lambda(
                Duration::from_secs(0)..Duration::from_secs(0),
                0.0,
                0.0,
                hold,
            )
            .await;
            let service = service_with_quiet(28, CAP, quiet);
            let (wall, _) = drive(&service, &api_base, CALLS).await;
            println!(
                "{name} | {:.2}s | {}",
                wall.as_secs_f64(),
                counts.requests.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}
