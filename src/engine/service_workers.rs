//! A scan's services analysed side by side, each on a thread of its own
//! (carrick#1066).
//!
//! Services used to be analysed one after another, so a large first index
//! kept one or two file analyses in flight against a process-wide cap many
//! times that, and each service's local work (its sidecar starting, discovery,
//! signatures, capture) was time with no request outstanding.
//!
//! - [`Plan`] decides how many may run at once and the memory budget they
//!   share, from the machine and [`SERVICE_CONCURRENCY_ENV`]; each service's
//!   [`ServiceMemory`] is its heap cap and its reservation against that
//!   budget, from the memory peak its previous generation stored
//!   (carrick#3084).
//! - [`in_declared_order`] admits services in declared order while each
//!   next one's reservation fits what the [`Ledger`] has left. It knows
//!   nothing of threads, so a replay on a paused clock drives the same
//!   admission the scan uses.
//! - [`walks_and_analyses`] admits a run's compiler walks and its analyses
//!   against the same ledger, each analysis once the walks it reads have
//!   ended (carrick#3085).
//! - [`on_a_thread`] runs one service's analysis on a plain `std::thread` that
//!   drives it with [`tokio::runtime::Handle::block_on`]. Not `tokio::spawn`:
//!   the analysis future is `!Send` (`swc_common::Lrc` is an `Rc`, and the
//!   file orchestrator holds a source map across its model awaits). A service
//!   blocked in a sidecar wait or a CPU pass holds its own thread, and the
//!   tasks it spawns run on the scan's runtime as before. The coordinator
//!   stays async, so the signal arm in `main` stays reachable.
//!
//! What a service writes depends on nothing here: each one's result is kept
//! at its declared index, and its blob is the same at every width and in
//! every order of finishing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::sync::Mutex;

use futures::StreamExt;
use futures::stream::FuturesUnordered;

use crate::services::machine;

/// Env override for how many services are analysed at once. `1` is one at a
/// time. A number above what the machine holds is honoured, so a runner or a
/// test can force a width. Unset, or `0`, sizes it from the machine.
/// Documented for runners in `docs/reference/runner.md`.
pub const SERVICE_CONCURRENCY_ENV: &str = "CARRICK_SERVICE_CONCURRENCY";

/// The most services analysed at once, whatever the machine holds.
const MAX_SERVICES_AT_ONCE: usize = 8;

/// What one service holds above its sidecar's V8 heap, in MB: the sidecar
/// process's own code and buffers, and the scanner's passes over the
/// service's sources (parsed modules, the call graph, the blob).
const SERVICE_OVERHEAD_MB: u64 = 1024;

/// V8's own old-space limit on a 64-bit machine, for a run whose
/// `CARRICK_SIDECAR_MAX_OLD_SPACE_MB` turned the cap off.
const V8_DEFAULT_HEAP_MB: u64 = 4096;

/// The smallest heap cap a service with a stored peak is started with.
const MIN_SERVICE_HEAP_MB: u64 = 2048;

/// Heap caps are rounded up to this many MB.
const HEAP_STEP_MB: u64 = 256;

/// The heap cap a walk with no stored peak starts with, unless the run's
/// cap is lower (carrick#3085).
pub const WALK_HEAP_CAP_MB: u64 = 6144;

/// Never fewer than this many at once, whatever the cores.
const CORES_FLOOR: usize = 2;

/// The stack each service's thread runs on: the main thread's, which the
/// analysis ran on before it had threads of its own. A spawned thread's
/// default is a quarter of it, and the parser recurses as deep as the source.
const SERVICE_STACK_BYTES: usize = 8 * 1024 * 1024;

fn forced_count() -> Option<usize> {
    std::env::var(SERVICE_CONCURRENCY_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
}

/// One service's memory in a pass (carrick#3084): the V8 heap cap its
/// sidecar starts with, and what it reserves of the run's budget while it
/// is analysed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceMemory {
    /// `None` starts its sidecar with V8's own limit.
    pub heap_cap_mb: Option<u64>,
    pub reservation_mb: u64,
    /// The peak its previous generation stored, which sized it.
    pub stored_peak_mb: Option<u64>,
}

impl ServiceMemory {
    /// Sized from `stored_peak_mb` under the run's cap `run_cap_mb`:
    /// - a stored peak `p`: a cap of 1.5 × `p` rounded up to 256 MB, no less
    ///   than [`MIN_SERVICE_HEAP_MB`] and no more than the run's cap;
    /// - no stored peak: the run's cap;
    /// - the run's cap turned off: no cap, reserved at V8's own limit.
    ///
    /// The reservation is the cap and [`SERVICE_OVERHEAD_MB`].
    pub fn sized(stored_peak_mb: Option<u64>, run_cap_mb: Option<u64>) -> Self {
        let heap_cap_mb = run_cap_mb.map(|run_cap| match stored_peak_mb {
            Some(peak) => ((peak * 3).div_ceil(2).div_ceil(HEAP_STEP_MB) * HEAP_STEP_MB)
                .max(MIN_SERVICE_HEAP_MB)
                .min(run_cap),
            None => run_cap,
        });
        ServiceMemory {
            heap_cap_mb,
            reservation_mb: heap_cap_mb.unwrap_or(V8_DEFAULT_HEAP_MB) + SERVICE_OVERHEAD_MB,
            stored_peak_mb,
        }
    }

    /// At the run's cap, whatever was stored: a service analysed again alone
    /// after a memory death, every service of a dispatched run, and a walk
    /// walked again after a memory death (carrick#3085).
    pub fn at_run_cap(run_cap_mb: Option<u64>) -> Self {
        Self::sized(None, run_cap_mb)
    }

    /// One service's compiler walk (carrick#3085), sized as an analysis is
    /// from the walk peak its previous generation stored, except with no
    /// stored peak: then [`WALK_HEAP_CAP_MB`], or the run's cap when that
    /// is lower. Walks peak far below analyses (most under 1 GB, the largest
    /// measured 3.2 GB), so a walk is never reserved a full analysis heap.
    pub fn for_walk(stored_walk_peak_mb: Option<u64>, run_cap_mb: Option<u64>) -> Self {
        match stored_walk_peak_mb {
            Some(_) => Self::sized(stored_walk_peak_mb, run_cap_mb),
            None => Self::sized(None, run_cap_mb.map(|cap| cap.min(WALK_HEAP_CAP_MB))),
        }
    }
}

/// How a pass admits services: how many at most at once, and the memory
/// budget their reservations share (carrick#3084).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The most in flight at once, never below one.
    most_at_once: usize,
    /// What reservations may add up to; `None` when the count alone decides
    /// ([`SERVICE_CONCURRENCY_ENV`], a dispatched run, a fixed width) or
    /// the free memory could not be read (then one at a time).
    budget_mb: Option<u64>,
    /// Why, for the width line.
    why: String,
}

impl Plan {
    /// The plan for this run's analysis, from the memory it can spend now
    /// ([`machine::available_memory`]), read once, its cores and
    /// [`SERVICE_CONCURRENCY_ENV`].
    ///
    /// A dispatched run analyses one at a time: whether a service builds its
    /// prompts for the job or generates its intents depends on whether the
    /// run has collected any prompt yet, a run-level fact that services side
    /// by side would race for.
    pub fn for_run(dispatching: bool) -> Self {
        Self::new(
            dispatching,
            machine::available_memory(),
            machine::cores(),
            forced_count(),
        )
    }

    /// [`Self::for_run`] with its inputs given.
    fn new(
        dispatching: bool,
        available: Option<machine::Available>,
        cores: usize,
        forced: Option<usize>,
    ) -> Self {
        if dispatching {
            return Self::count(1, "a dispatched run analyses one at a time");
        }
        if let Some(count) = forced {
            return Self::count(
                count,
                &format!("asked for by {SERVICE_CONCURRENCY_ENV}; the budget is not read"),
            );
        }
        let most = cores
            .saturating_sub(1)
            .clamp(CORES_FLOOR, MAX_SERVICES_AT_ONCE);
        match available {
            Some(available) => {
                let budget = available.mb.saturating_sub(machine::HEADROOM_MB);
                Plan {
                    most_at_once: most,
                    budget_mb: Some(budget),
                    why: format!(
                        "budget {budget} MB (limit from {}), at most {most} at once",
                        available.source.as_str()
                    ),
                }
            }
            None => Self::count(1, "the free memory could not be read"),
        }
    }

    /// At most `count` at once, whatever they reserve.
    pub fn count(count: usize, why: &str) -> Self {
        let count = count.max(1);
        Plan {
            most_at_once: count,
            budget_mb: None,
            why: format!("at most {count} at once: {why}"),
        }
    }

    /// One at a time: the pass that analyses a service again alone.
    pub fn alone() -> Self {
        Self::count(1, "alone")
    }

    /// The most in flight at once.
    pub fn most_at_once(&self) -> usize {
        self.most_at_once
    }

    /// Whether a service reserving `reservation_mb` may start while
    /// `in_flight` services reserve `reserved_mb`: always when none is in
    /// flight (a service alone starts whatever it reserves); otherwise only
    /// below the count, and, when there is a budget, while it fits what is
    /// left of it.
    fn may_start(&self, reservation_mb: u64, in_flight: usize, reserved_mb: u64) -> bool {
        if in_flight == 0 {
            return true;
        }
        if in_flight >= self.most_at_once {
            return false;
        }
        match self.budget_mb {
            Some(budget) => reserved_mb + reservation_mb <= budget,
            None => true,
        }
    }

    /// The width line: the budget and where its limit came from, the most at
    /// once, and how many of `services` were sized by a stored peak and how
    /// many start at the run's full cap `run_cap_mb`.
    pub fn line(&self, services: &[ServiceMemory], run_cap_mb: Option<u64>) -> String {
        let with_peak = services
            .iter()
            .filter(|service| service.stored_peak_mb.is_some())
            .count();
        let at_full_cap = services
            .iter()
            .filter(|service| service.heap_cap_mb == run_cap_mb)
            .count();
        format!(
            "Analysing services by memory: {}; {with_peak} with a stored peak, {at_full_cap} at \
             the full cap",
            self.why
        )
    }
}

/// What the services in flight reserve of the run's budget, and how many
/// they are (carrick#3084). One object for the run: every pass of the
/// analysis admits against it, and a memory death reads it to say whether
/// another service was in flight ([`run_ledger`]).
#[derive(Debug, Default)]
pub struct Ledger {
    held: Mutex<Held>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Held {
    in_flight: usize,
    reserved_mb: u64,
}

impl Ledger {
    pub const fn new() -> Self {
        Ledger {
            held: Mutex::new(Held {
                in_flight: 0,
                reserved_mb: 0,
            }),
        }
    }

    /// How many are in flight now.
    pub fn in_flight(&self) -> usize {
        self.held.lock().map(|held| held.in_flight).unwrap_or(0)
    }

    /// Take `reservation_mb` when `plan` lets it start, or when `alone`: the
    /// admitter has nothing of its own in flight, so a wait would never end.
    fn try_take(&self, plan: &Plan, reservation_mb: u64, alone: bool) -> bool {
        let Ok(mut held) = self.held.lock() else {
            return alone;
        };
        if !alone && !plan.may_start(reservation_mb, held.in_flight, held.reserved_mb) {
            return false;
        }
        held.in_flight += 1;
        held.reserved_mb += reservation_mb;
        true
    }

    fn give_back(&self, reservation_mb: u64) {
        if let Ok(mut held) = self.held.lock() {
            held.in_flight = held.in_flight.saturating_sub(1);
            held.reserved_mb = held.reserved_mb.saturating_sub(reservation_mb);
        }
    }
}

static RUN_LEDGER: Ledger = Ledger::new();

/// The run's ledger: what the scan's analysis admits against.
pub fn run_ledger() -> &'static Ledger {
    &RUN_LEDGER
}

/// How many services with no stored peak, each at the run's cap
/// `run_cap_mb`, a machine with `available_mb` free and `cores` cores holds
/// at once, for a test elsewhere in the crate that sets a sidecar heap cap
/// and reads what it buys.
#[cfg(test)]
pub(crate) fn at_once_with(available_mb: u64, cores: usize, run_cap_mb: Option<u64>) -> usize {
    let plan = Plan::new(
        false,
        Some(machine::Available {
            mb: available_mb,
            source: machine::MemorySource::Env,
        }),
        cores,
        None,
    );
    let reservation = ServiceMemory::sized(None, run_cap_mb).reservation_mb;
    let ledger = Ledger::new();
    let mut held = 0;
    while held < plan.most_at_once && ledger.try_take(&plan, reservation, held == 0) {
        held += 1;
    }
    held
}

/// How one pass of services was scheduled, for a test to read: proof that
/// services really ran side by side and finished out of order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    /// The most the plan let run at once.
    pub width: usize,
    /// The most services in flight at once.
    pub most_in_flight: usize,
    /// Declared indices, in the order the services finished.
    pub finished: Vec<usize>,
}

static SCHEDULES: Mutex<Vec<Schedule>> = Mutex::new(Vec::new());

/// Every pass scheduled since the last call, in order (the loop, then the
/// owed-work retry when it ran).
pub fn take_schedules() -> Vec<Schedule> {
    SCHEDULES
        .lock()
        .map(|mut passes| std::mem::take(&mut *passes))
        .unwrap_or_default()
}

/// Run one job per entry of `reservations`, each started in declared order
/// while `plan` and what `ledger` has left let it start, and return their
/// results by index. A job's reservation is given back when it ends.
///
/// The order is strict: a job that does not fit holds the ones behind it,
/// so a large one is never starved, and the order of starts depends only on
/// the order of finishing.
///
/// The admission and nothing else: `start` decides where a job runs.
pub async fn in_declared_order<R, Fut>(
    ledger: &Ledger,
    plan: &Plan,
    reservations: &[u64],
    start: impl FnMut(usize) -> Fut,
) -> Vec<R>
where
    Fut: Future<Output = R>,
{
    let (results, schedule) = admit(ledger, plan, reservations, start).await;
    if let Ok(mut passes) = SCHEDULES.lock() {
        passes.push(schedule);
    }
    results
}

async fn admit<R, Fut>(
    ledger: &Ledger,
    plan: &Plan,
    reservations: &[u64],
    mut start: impl FnMut(usize) -> Fut,
) -> (Vec<R>, Schedule)
where
    Fut: Future<Output = R>,
{
    let count = reservations.len();
    let mut results: Vec<Option<R>> = (0..count).map(|_| None).collect();
    let mut schedule = Schedule {
        width: plan.most_at_once,
        ..Schedule::default()
    };
    let mut running = FuturesUnordered::new();
    let mut next = 0;
    loop {
        while next < count && ledger.try_take(plan, reservations[next], running.is_empty()) {
            let index = next;
            next += 1;
            let job = start(index);
            running.push(async move { (index, job.await) });
            schedule.most_in_flight = schedule.most_in_flight.max(running.len());
        }
        let Some((index, result)) = running.next().await else {
            break;
        };
        ledger.give_back(reservations[index]);
        schedule.finished.push(index);
        results[index] = Some(result);
    }
    let results = results
        .into_iter()
        .map(|result| result.expect("every admitted job ends with a result"))
        .collect();
    (results, schedule)
}

/// What a walk's end tells [`walks_and_analyses`] (carrick#3085).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WalkEnd {
    /// Its rows are in its slot, and these are the services they reach.
    Walked { reaches: BTreeSet<usize> },
    /// It died of memory below the run's cap: walk it again once, alone,
    /// reserving this much.
    Again { reservation_mb: u64 },
}

/// What [`walks_and_analyses`] does with a service another's rows reach
/// that has no walk of its own in the pass: a carried one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Reached {
    /// Its slot is filled without a walk (rows an earlier pass kept), and
    /// these are the services they reach.
    Filled { reaches: BTreeSet<usize> },
    /// Walk it, reserving this much.
    Walk { reservation_mb: u64 },
}

/// The work of a pass whose analyses wait for walks (carrick#3085). Services
/// are named by their declared position; analyses by their position in the
/// pass. Every future is started on the scan's runtime and owns what it
/// reads.
pub(super) trait WalkedPass {
    type Walk: Send + 'static;
    type Analysed: Send + 'static;
    /// Start `service`'s walk; `again` once it died of memory.
    fn start_walk(
        &mut self,
        service: usize,
        again: bool,
    ) -> futures::future::BoxFuture<'static, Self::Walk>;
    /// `service`'s walk ended with `walk`. A walk `again` is never walked
    /// a third time: [`WalkEnd::Again`] from one is read as walked, reaching
    /// nothing.
    fn walk_ended(&mut self, service: usize, again: bool, walk: Self::Walk) -> WalkEnd;
    /// `service` was first reached, by the rows of `by`.
    fn reached(&mut self, service: usize, by: usize) -> Reached;
    /// Start the pass's analysis at `position`, whose gate opened on the
    /// walks of `reads` (its own among them).
    fn start_analysis(
        &mut self,
        position: usize,
        reads: &BTreeSet<usize>,
    ) -> futures::future::BoxFuture<'static, Self::Analysed>;
}

/// Where a service's walk is in a [`walks_and_analyses`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkState {
    /// Queued or running.
    Pending,
    Walked,
}

/// A walk in the queue.
#[derive(Debug, Clone, Copy)]
struct QueuedWalk {
    service: usize,
    reservation_mb: u64,
    /// Walked again after a memory death: admitted only with nothing else
    /// in flight.
    again: bool,
}

enum Ended<W, R> {
    Walk {
        service: usize,
        again: bool,
        reservation_mb: u64,
        walk: W,
    },
    Analysis {
        position: usize,
        reservation_mb: u64,
        analysed: R,
    },
}

/// A pass's walks and analyses, admitted together against `ledger` under
/// `plan`, one budget and one count for both (carrick#3085):
///
/// - `walks` are queued in the order given (declared order), each with its
///   reservation. Walks gate analyses, so whenever budget frees the head of
///   the walk queue is tried first; only while it does not fit may a ready
///   analysis that does start.
/// - The analysis at position `p` of `analyses` (its service, its
///   reservation) is ready once its service's walk has ended and so has the
///   walk of every service its rows reach, closed over each reached
///   service's own rows. A carried service is reached through
///   [`WalkedPass::reached`] the first time any rows name it: walked once
///   for the pass, or filled. Ready analyses are admitted in their order
///   given; one that does not fit holds the ones behind it. An analysis
///   reserves nothing before it is ready, so none holds memory while it
///   waits for a walk.
/// - A walk that died of memory ([`WalkEnd::Again`]) is queued again at the
///   tail, and admitted only once nothing else is in flight.
///
/// Returns each analysis's result at its position. The schedule of the
/// analyses is the one [`take_schedules`] returns.
pub(super) async fn walks_and_analyses<P: WalkedPass>(
    ledger: &Ledger,
    plan: &Plan,
    walks: &[(usize, u64)],
    analyses: &[(usize, u64)],
    pass: &mut P,
) -> Vec<P::Analysed> {
    let mut state: BTreeMap<usize, WalkState> = BTreeMap::new();
    let mut reaches: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut queue: VecDeque<QueuedWalk> = VecDeque::new();
    for &(service, reservation_mb) in walks {
        state.insert(service, WalkState::Pending);
        queue.push_back(QueuedWalk {
            service,
            reservation_mb,
            again: false,
        });
    }
    let mut waiting: Vec<usize> = (0..analyses.len()).collect();
    let mut results: Vec<Option<P::Analysed>> = (0..analyses.len()).map(|_| None).collect();
    let mut schedule = Schedule {
        width: plan.most_at_once,
        ..Schedule::default()
    };
    let mut analyses_in_flight = 0usize;
    let mut running = FuturesUnordered::new();
    loop {
        loop {
            if let Some(head) = queue.front().copied() {
                let alone = running.is_empty();
                let admitted =
                    (!head.again || alone) && ledger.try_take(plan, head.reservation_mb, alone);
                if admitted {
                    queue.pop_front();
                    let job = pass.start_walk(head.service, head.again);
                    running.push(futures::future::Either::Left(async move {
                        Ended::Walk {
                            service: head.service,
                            again: head.again,
                            reservation_mb: head.reservation_mb,
                            walk: job.await,
                        }
                    }));
                    continue;
                }
            }
            // The walk at the head does not fit beside what is in flight: a
            // ready analysis that does may start meanwhile.
            let ready = waiting.iter().enumerate().find_map(|(at, &position)| {
                gate_open(analyses[position].0, &state, &reaches).map(|reads| (at, reads))
            });
            // Nothing in flight, nothing queued and nothing ready: a gate
            // names a walk the pass will never end. Never wait on it.
            let stalled =
                ready.is_none() && running.is_empty() && queue.is_empty() && !waiting.is_empty();
            debug_assert!(
                !stalled,
                "an analysis waits for a walk nothing will end: {waiting:?}"
            );
            let Some((at, reads)) = ready.or(stalled.then(|| (0, BTreeSet::new()))) else {
                break;
            };
            let position = waiting[at];
            let reservation_mb = analyses[position].1;
            if !ledger.try_take(plan, reservation_mb, running.is_empty()) {
                break;
            }
            waiting.remove(at);
            let job = pass.start_analysis(position, &reads);
            running.push(futures::future::Either::Right(async move {
                Ended::Analysis {
                    position,
                    reservation_mb,
                    analysed: job.await,
                }
            }));
            analyses_in_flight += 1;
            schedule.most_in_flight = schedule.most_in_flight.max(analyses_in_flight);
        }
        let Some(ended) = running.next().await else {
            break;
        };
        match ended {
            Ended::Walk {
                service,
                again,
                reservation_mb,
                walk,
            } => {
                ledger.give_back(reservation_mb);
                match pass.walk_ended(service, again, walk) {
                    WalkEnd::Again { reservation_mb } if !again => {
                        queue.push_back(QueuedWalk {
                            service,
                            reservation_mb,
                            again: true,
                        });
                    }
                    WalkEnd::Again { .. } => {
                        state.insert(service, WalkState::Walked);
                    }
                    WalkEnd::Walked { reaches: reached } => {
                        state.insert(service, WalkState::Walked);
                        reach_from(service, reached, pass, &mut state, &mut reaches, &mut queue);
                    }
                }
            }
            Ended::Analysis {
                position,
                reservation_mb,
                analysed,
            } => {
                ledger.give_back(reservation_mb);
                analyses_in_flight -= 1;
                schedule.finished.push(position);
                results[position] = Some(analysed);
            }
        }
    }
    if let Ok(mut passes) = SCHEDULES.lock() {
        passes.push(schedule);
    }
    results
        .into_iter()
        .map(|result| result.expect("every analysis is admitted and ends with a result"))
        .collect()
}

/// Record that `by`'s rows reach `reached`, and reach every service among
/// them the pass has not met yet: queue its walk, or record the rows it was
/// filled with, and reach on from those in turn.
fn reach_from<P: WalkedPass>(
    by: usize,
    reached: BTreeSet<usize>,
    pass: &mut P,
    state: &mut BTreeMap<usize, WalkState>,
    reaches: &mut BTreeMap<usize, BTreeSet<usize>>,
    queue: &mut VecDeque<QueuedWalk>,
) {
    let mut met: Vec<(usize, usize)> = reached
        .iter()
        .filter(|service| !state.contains_key(service))
        .map(|service| (*service, by))
        .collect();
    reaches.insert(by, reached);
    while let Some((service, by)) = met.pop() {
        if state.contains_key(&service) {
            continue;
        }
        match pass.reached(service, by) {
            Reached::Walk { reservation_mb } => {
                state.insert(service, WalkState::Pending);
                queue.push_back(QueuedWalk {
                    service,
                    reservation_mb,
                    again: false,
                });
            }
            Reached::Filled { reaches: further } => {
                state.insert(service, WalkState::Walked);
                met.extend(
                    further
                        .iter()
                        .filter(|next| !state.contains_key(next))
                        .map(|next| (*next, service)),
                );
                reaches.insert(service, further);
            }
        }
    }
}

/// The walks `service`'s analysis reads, once they have all ended: its own,
/// and the walk of every service its rows reach, closed over each reached
/// service's rows. `None` while any has not ended.
fn gate_open(
    service: usize,
    state: &BTreeMap<usize, WalkState>,
    reaches: &BTreeMap<usize, BTreeSet<usize>>,
) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![service];
    while let Some(next) = pending.pop() {
        if !seen.insert(next) {
            continue;
        }
        if state.get(&next) != Some(&WalkState::Walked) {
            return None;
        }
        if let Some(further) = reaches.get(&next) {
            pending.extend(further.iter().copied());
        }
    }
    Some(seen)
}

/// Run `work` on a thread of its own and await what it returns.
///
/// `work` is handed the scan's runtime, to drive its futures with
/// `block_on` on this thread: nothing it runs may call `block_on` itself. A
/// panic in it is caught there, after the panic hook has reported it, and
/// raised again here, so a panic still ends the run.
pub fn on_a_thread<R, W>(name: String, work: W) -> impl Future<Output = R>
where
    R: Send + 'static,
    W: FnOnce(&tokio::runtime::Handle) -> R + Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name(name)
        .stack_size(SERVICE_STACK_BYTES)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&handle)));
            let _ = sender.send(outcome);
        });
    async move {
        if let Err(error) = spawned {
            panic!("a service's thread could not be started: {error}");
        }
        match receiver.await {
            Ok(Ok(result)) => result,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => panic!("a service's thread ended without a result"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const GB: u64 = 1024;

    /// What the budget holds of services with no stored peak, one row per
    /// input that decides it.
    #[test]
    fn the_budget_holds_what_the_memory_and_cores_allow() {
        // 32 GB free, 16 cores, the 8 GB heap: (32 - 2) / 9 holds 3.
        assert_eq!(at_once_with(32 * GB, 16, Some(8 * GB)), 3);
        // A laptop with 12 GB free at the 8 GB heap: one.
        assert_eq!(at_once_with(12 * GB, 10, Some(8 * GB)), 1);
        // A runner with 64 GB and a 3 GB heap per shape: cores decide.
        assert_eq!(at_once_with(64 * GB, 4, Some(3 * GB)), 3);
        // Two cores still run two: the floor.
        assert_eq!(at_once_with(64 * GB, 1, Some(3 * GB)), 2);
        // Never more than eight.
        assert_eq!(at_once_with(512 * GB, 64, Some(2 * GB)), 8);
        // Too little memory for one: one, never zero.
        assert_eq!(at_once_with(GB, 16, Some(8 * GB)), 1);
        // The heap cap turned off: V8's own default is the size.
        assert_eq!(at_once_with(17 * GB, 16, None), 3);
    }

    /// carrick#3085: a walk is sized by its stored walk peak as an analysis
    /// is by its own; with none, 6 GB or the run's cap when that is lower.
    #[test]
    fn a_walk_is_capped_by_its_stored_peak_or_six_gb() {
        let walk = |peak, run| ServiceMemory::for_walk(peak, run);
        assert_eq!(walk(Some(500), Some(12_288)).heap_cap_mb, Some(2_048));
        assert_eq!(walk(Some(3_177), Some(12_288)).heap_cap_mb, Some(4_864));
        assert_eq!(walk(None, Some(12_288)).heap_cap_mb, Some(6_144));
        assert_eq!(walk(None, Some(12_288)).reservation_mb, 7_168);
        // Never above the run's cap.
        assert_eq!(walk(None, Some(4_096)).heap_cap_mb, Some(4_096));
        assert_eq!(walk(Some(9_000), Some(4_096)).heap_cap_mb, Some(4_096));
        // The run's cap turned off: none, reserved at V8's own limit.
        assert_eq!(walk(None, None).heap_cap_mb, None);
        assert_eq!(walk(None, None).reservation_mb, 5_120);
    }

    /// One job of the stand-in pass, as it ran: what, when, reserving what.
    #[derive(Debug, Clone)]
    struct Ran {
        what: String,
        started: Duration,
        ended: Duration,
        reservation_mb: u64,
    }

    /// A pass of sleeps on a paused clock (carrick#3085). Service `s` walks
    /// for `walk_secs[s]` seconds and its rows reach `reaches[s]`; the
    /// analysis at position `p` runs `analysis_secs[p]` seconds. A service
    /// in `dies_once` dies of memory on its first walk.
    struct StandIn {
        began: tokio::time::Instant,
        walk_secs: Vec<u64>,
        walk_mb: Vec<u64>,
        reaches: Vec<BTreeSet<usize>>,
        analysis_secs: Vec<u64>,
        analysis_mb: Vec<u64>,
        dies_once: BTreeSet<usize>,
        again_mb: u64,
        ran: std::sync::Arc<Mutex<Vec<Ran>>>,
        reached_by: Vec<(usize, usize)>,
    }

    impl StandIn {
        fn job(
            &self,
            what: String,
            secs: u64,
            reservation_mb: u64,
        ) -> futures::future::BoxFuture<'static, ()> {
            let (began, ran) = (self.began, std::sync::Arc::clone(&self.ran));
            Box::pin(async move {
                let started = began.elapsed();
                tokio::time::sleep(Duration::from_secs(secs)).await;
                ran.lock().unwrap().push(Ran {
                    what,
                    started,
                    ended: began.elapsed(),
                    reservation_mb,
                });
            })
        }

        fn ran(&self) -> Vec<Ran> {
            self.ran.lock().unwrap().clone()
        }

        fn one(&self, what: &str) -> Ran {
            let ran = self.ran();
            let found: Vec<&Ran> = ran.iter().filter(|ran| ran.what == what).collect();
            assert_eq!(found.len(), 1, "{what}: {ran:?}");
            found[0].clone()
        }
    }

    impl WalkedPass for StandIn {
        type Walk = ();
        type Analysed = usize;

        fn start_walk(
            &mut self,
            service: usize,
            again: bool,
        ) -> futures::future::BoxFuture<'static, ()> {
            let mb = if again {
                self.again_mb
            } else {
                self.walk_mb[service]
            };
            let what = format!("walk {service}{}", if again { " again" } else { "" });
            self.job(what, self.walk_secs[service], mb)
        }

        fn walk_ended(&mut self, service: usize, again: bool, _: ()) -> WalkEnd {
            if !again && self.dies_once.contains(&service) {
                return WalkEnd::Again {
                    reservation_mb: self.again_mb,
                };
            }
            WalkEnd::Walked {
                reaches: self.reaches[service].clone(),
            }
        }

        fn reached(&mut self, service: usize, by: usize) -> Reached {
            self.reached_by.push((service, by));
            Reached::Walk {
                reservation_mb: self.walk_mb[service],
            }
        }

        fn start_analysis(
            &mut self,
            position: usize,
            _: &BTreeSet<usize>,
        ) -> futures::future::BoxFuture<'static, usize> {
            let job = self.job(
                format!("analysis {position}"),
                self.analysis_secs[position],
                self.analysis_mb[position],
            );
            Box::pin(async move {
                job.await;
                position * 10
            })
        }
    }

    fn stand_in(services: usize) -> StandIn {
        StandIn {
            began: tokio::time::Instant::now(),
            walk_secs: vec![1; services],
            walk_mb: vec![0; services],
            reaches: vec![BTreeSet::new(); services],
            analysis_secs: vec![1; services],
            analysis_mb: vec![0; services],
            dies_once: BTreeSet::new(),
            again_mb: 0,
            ran: Default::default(),
            reached_by: Vec::new(),
        }
    }

    /// carrick#3085, the gate: the first service's rows reach the second,
    /// whose walk is the slower. The first's analysis is not admitted when
    /// its own walk ends, but when the second's does; the second's own
    /// analysis is admitted at the same moment, and neither waited for a
    /// barrier over every walk (the third's walk is slower still).
    #[tokio::test(start_paused = true)]
    async fn an_analysis_waits_for_the_walks_its_rows_reach() {
        take_schedules();
        let mut pass = stand_in(3);
        pass.walk_secs = vec![1, 10, 30];
        pass.reaches[0] = BTreeSet::from([1]);
        pass.analysis_secs = vec![5, 5, 5];
        let plan = Plan::count(4, "the test's");
        let results = walks_and_analyses(
            &Ledger::new(),
            &plan,
            &[(0, 0), (1, 0), (2, 0)],
            &[(0, 0), (1, 0), (2, 0)],
            &mut pass,
        )
        .await;
        assert_eq!(results, vec![0, 10, 20]);
        let second_walked = pass.one("walk 1").ended;
        assert_eq!(second_walked, Duration::from_secs(10));
        assert_eq!(
            pass.one("analysis 0").started,
            second_walked,
            "{:?}",
            pass.ran()
        );
        assert_eq!(pass.one("analysis 1").started, second_walked);
        assert!(pass.one("analysis 0").started < pass.one("walk 2").ended);
        take_schedules();
    }

    /// carrick#3085, the closure: a carried service the first's rows reach
    /// is walked on demand, once, though two services reach it, and a
    /// service it reaches in turn is walked too; both analyses wait for
    /// every walk in their closure.
    #[tokio::test(start_paused = true)]
    async fn a_carried_service_is_walked_once_for_every_service_that_reaches_it() {
        take_schedules();
        let mut pass = stand_in(4);
        // 0 and 1 are analysed; 2 and 3 are carried.
        pass.walk_secs = vec![1, 2, 4, 8];
        pass.reaches[0] = BTreeSet::from([2]);
        pass.reaches[1] = BTreeSet::from([2]);
        pass.reaches[2] = BTreeSet::from([3]);
        let plan = Plan::count(8, "the test's");
        walks_and_analyses(
            &Ledger::new(),
            &plan,
            &[(0, 0), (1, 0)],
            &[(0, 0), (1, 0)],
            &mut pass,
        )
        .await;
        assert_eq!(pass.reached_by, vec![(2, 0), (3, 2)]);
        let last = pass.one("walk 3").ended;
        assert_eq!(pass.one("walk 2").ended, Duration::from_secs(5));
        assert_eq!(last, Duration::from_secs(13));
        assert_eq!(pass.one("analysis 0").started, last);
        assert_eq!(pass.one("analysis 1").started, last);
        take_schedules();
    }

    /// The sum the jobs in flight reserved at each moment one started, with
    /// how many were in flight then and whether a walk and an analysis were
    /// among them.
    fn reserved_at_each_start(ran: &[Ran]) -> Vec<(u64, usize, bool)> {
        ran.iter()
            .map(|start| {
                let in_flight: Vec<&Ran> = ran
                    .iter()
                    .filter(|ran| ran.started <= start.started && start.started < ran.ended)
                    .collect();
                let kinds = (
                    in_flight.iter().any(|ran| ran.what.starts_with("walk")),
                    in_flight.iter().any(|ran| ran.what.starts_with("analysis")),
                );
                (
                    in_flight.iter().map(|ran| ran.reservation_mb).sum(),
                    in_flight.len(),
                    kinds.0 && kinds.1,
                )
            })
            .collect()
    }

    /// carrick#3085, one budget: walks and analyses report what they
    /// reserved, and what is in flight together never exceeds the budget,
    /// except for one job alone; walks and analyses did run side by side
    /// under it. A walk that died of memory is walked again alone, after
    /// the walks queued with it.
    #[tokio::test(start_paused = true)]
    async fn walks_and_analyses_share_one_budget() {
        take_schedules();
        let plan = plan_under(16_384, 1_700, 8, None);
        let budget = plan.budget_mb.unwrap();
        let mut pass = stand_in(6);
        pass.walk_secs = vec![3, 1, 6, 2, 5, 4];
        pass.walk_mb = vec![7_168, 3_072, 7_168, 3_072, 7_168, 3_072];
        pass.analysis_secs = vec![9, 4, 7, 3, 8, 2];
        pass.analysis_mb = vec![5_120, 7_424, 3_072, 13_312, 3_072, 5_120];
        pass.reaches[1] = BTreeSet::from([5]);
        pass.dies_once = BTreeSet::from([1]);
        pass.again_mb = 13_312;
        let walks: Vec<(usize, u64)> = (0..5).map(|s| (s, pass.walk_mb[s])).collect();
        let analyses: Vec<(usize, u64)> = (0..5).map(|s| (s, pass.analysis_mb[s])).collect();
        let ledger = Ledger::new();
        walks_and_analyses(&ledger, &plan, &walks, &analyses, &mut pass).await;
        assert_eq!(ledger.in_flight(), 0, "every reservation is given back");
        let ran = pass.ran();
        assert_eq!(ran.len(), 5 + 1 + 1 + 5, "{ran:#?}");
        let at_starts = reserved_at_each_start(&ran);
        for (reserved, in_flight, _) in &at_starts {
            assert!(
                *in_flight == 1 || *reserved <= budget,
                "{reserved} MB reserved by {in_flight} in flight, over {budget}: {ran:#?}"
            );
        }
        assert!(
            at_starts.iter().any(|(_, _, both)| *both),
            "no walk ever ran beside an analysis: {ran:#?}"
        );
        // The walk again: alone, after every walk queued before it ended.
        let again = pass.one("walk 1 again");
        for other in ran.iter().filter(|ran| ran.what != "walk 1 again") {
            assert!(
                other.ended <= again.started || other.started >= again.ended,
                "{} ran beside the walk again: {ran:#?}",
                other.what
            );
        }
        for queued in ["walk 0", "walk 2", "walk 3", "walk 4"] {
            assert!(pass.one(queued).started <= again.started, "{ran:#?}");
        }
        // Its analysis waited for it, and for the carried walk its rows reach.
        assert!(pass.one("analysis 1").started >= again.ended);
        assert!(pass.one("analysis 1").started >= pass.one("walk 5").ended);
        take_schedules();
    }

    /// Jobs start in declared order, no more than the width at once, and
    /// each result is kept at its own index whatever order they end in.
    #[tokio::test(start_paused = true)]
    async fn jobs_start_in_declared_order_and_keep_their_index() {
        take_schedules();
        let took = [5u64, 1, 2, 1];
        let started = std::cell::RefCell::new(Vec::new());
        let plan = Plan::count(2, "the test's");
        let results = in_declared_order(&Ledger::new(), &plan, &[0; 4], |index| {
            started.borrow_mut().push(index);
            async move {
                tokio::time::sleep(Duration::from_secs(took[index])).await;
                index * 10
            }
        })
        .await;
        assert_eq!(results, vec![0, 10, 20, 30]);
        assert_eq!(*started.borrow(), vec![0, 1, 2, 3]);
        let passes = take_schedules();
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].most_in_flight, 2);
        // 1 ends at 1s and 2 starts; 2 ends at 3s and 3 starts; 3 ends at
        // 4s; 0 at 5s.
        assert_eq!(passes[0].finished, vec![1, 2, 3, 0]);
    }

    /// The analysis sidecar peaks, in MB and declared order, of the 35
    /// services of a pilot copy analysed one at a time (plan ticket M3).
    const PILOT_PEAKS: [u64; 35] = [
        1120, 1739, 4159, 399, 535, 795, 522, 845, 493, 405, 470, 490, 846, 401, 539, 492, 497,
        516, 455, 850, 848, 937, 403, 434, 450, 598, 941, 950, 606, 943, 925, 1694, 400, 4088, 938,
    ];

    /// The sets of services in flight, one per start, when `memories` are
    /// admitted under `plan`, each taking a different few seconds.
    async fn in_flight_at_each_start(
        plan: &Plan,
        memories: &[ServiceMemory],
    ) -> Vec<std::collections::BTreeSet<usize>> {
        let running = std::cell::RefCell::new(std::collections::BTreeSet::new());
        let seen = std::cell::RefCell::new(Vec::new());
        let reservations: Vec<u64> = memories.iter().map(|m| m.reservation_mb).collect();
        let ledger = Ledger::new();
        in_declared_order(&ledger, plan, &reservations, |index| {
            running.borrow_mut().insert(index);
            seen.borrow_mut().push(running.borrow().clone());
            let running = &running;
            async move {
                tokio::time::sleep(Duration::from_secs(5 + (index as u64 * 7) % 11)).await;
                running.borrow_mut().remove(&index);
            }
        })
        .await;
        assert_eq!(ledger.in_flight(), 0, "every reservation is given back");
        seen.into_inner()
    }

    /// What the run may spend, as a runner that sets the env reads it:
    /// `limit` less what the scanner and its sidecars hold.
    fn plan_under(limit: u64, held: u64, cores: usize, forced: Option<usize>) -> Plan {
        Plan::new(
            false,
            Some(machine::Available {
                mb: limit - held,
                source: machine::MemorySource::Env,
            }),
            cores,
            forced,
        )
    }

    fn most_in_flight(starts: &[std::collections::BTreeSet<usize>]) -> usize {
        starts.iter().map(|set| set.len()).max().unwrap_or(0)
    }

    /// carrick#3084, the admission table: a 16 GB task with 1.7 GB held,
    /// the run's cap at 12 GB and 4 cores. Sized by the pilot's stored peaks,
    /// its two largest services never run at once and the others run up to
    /// three at a time. With no stored peaks every service reserves the full
    /// cap, and they run one at a time, as they did.
    #[tokio::test(start_paused = true)]
    async fn services_are_admitted_by_their_stored_peaks() {
        take_schedules();
        let run_cap = Some(12_288);
        let plan = plan_under(16_384, 1_700, 4, None);
        assert_eq!(plan.budget_mb, Some(12_636));
        let sized: Vec<ServiceMemory> = PILOT_PEAKS
            .iter()
            .map(|peak| ServiceMemory::sized(Some(*peak), run_cap))
            .collect();
        let starts = in_flight_at_each_start(&plan, &sized).await;
        assert_eq!(starts.len(), 35, "every service starts once");
        let (largest, next) = (2, 33);
        assert_eq!((PILOT_PEAKS[largest], PILOT_PEAKS[next]), (4159, 4088));
        assert!(
            starts
                .iter()
                .all(|set| !(set.contains(&largest) && set.contains(&next))),
            "the two largest ran at once: {starts:?}"
        );
        assert_eq!(most_in_flight(&starts), 3, "{starts:?}");

        let peakless: Vec<ServiceMemory> = PILOT_PEAKS
            .iter()
            .map(|_| ServiceMemory::sized(None, run_cap))
            .collect();
        let starts = in_flight_at_each_start(&plan, &peakless).await;
        assert_eq!(most_in_flight(&starts), 1, "{starts:?}");
        take_schedules();
    }

    /// An 8 GB task at a 6 GB run cap holds one of the pilot's services at a
    /// time, sized or not.
    #[tokio::test(start_paused = true)]
    async fn an_eight_gb_task_analyses_one_at_a_time() {
        take_schedules();
        let run_cap = Some(6_144);
        let plan = plan_under(8_192, 1_700, 4, None);
        let sized: Vec<ServiceMemory> = PILOT_PEAKS
            .iter()
            .map(|peak| ServiceMemory::sized(Some(*peak), run_cap))
            .collect();
        let starts = in_flight_at_each_start(&plan, &sized).await;
        assert_eq!(most_in_flight(&starts), 1, "{starts:?}");
        take_schedules();
    }

    /// A service with no stored peak reserves the full cap: among services
    /// with peaks it starts only when nothing else is in flight, and the ones
    /// behind it wait for it, in declared order.
    #[tokio::test(start_paused = true)]
    async fn a_service_with_no_peak_starts_only_alone() {
        take_schedules();
        let run_cap = Some(12_288);
        let plan = plan_under(16_384, 1_700, 4, None);
        let peaks = [Some(400), Some(900), None, Some(400), Some(600), Some(500)];
        let memories: Vec<ServiceMemory> = peaks
            .iter()
            .map(|peak| ServiceMemory::sized(*peak, run_cap))
            .collect();
        let starts = in_flight_at_each_start(&plan, &memories).await;
        for set in &starts {
            if set.contains(&2) {
                assert_eq!(
                    set.len(),
                    1,
                    "the peakless one ran beside another: {starts:?}"
                );
            }
        }
        // Strict order: none behind it started before it.
        let started_alone = starts.iter().position(|set| set.contains(&2)).unwrap();
        assert_eq!(started_alone, 2, "{starts:?}");
        assert!(most_in_flight(&starts) >= 2, "{starts:?}");
        take_schedules();
    }

    /// The knob forces the count, the budget unread; the caps stay derived.
    #[tokio::test(start_paused = true)]
    async fn the_knob_forces_the_count_whatever_the_budget() {
        take_schedules();
        let plan = plan_under(16_384, 1_700, 4, Some(3));
        let peakless = vec![ServiceMemory::sized(None, Some(12_288)); 6];
        let starts = in_flight_at_each_start(&plan, &peakless).await;
        assert_eq!(most_in_flight(&starts), 3, "{starts:?}");
        // A dispatched run: one, whatever is asked.
        let dispatched = Plan::new(true, None, 64, Some(4));
        assert_eq!(dispatched.most_at_once(), 1);
        // Memory that cannot be read: one.
        assert_eq!(Plan::new(false, None, 16, None).most_at_once(), 1);
        take_schedules();
    }

    /// A service's cap is 1.5 times its stored peak, rounded up to 256 MB,
    /// between 2 GB and the run's cap; with no peak, the run's cap; with the
    /// run's cap off, none. Each reserves its cap and 1 GB.
    #[test]
    fn a_service_is_capped_by_its_stored_peak() {
        let cap = |peak, run| ServiceMemory::sized(peak, run).heap_cap_mb;
        assert_eq!(cap(Some(1_000), Some(12_288)), Some(2_048));
        assert_eq!(cap(Some(1_739), Some(12_288)), Some(2_816));
        assert_eq!(cap(Some(4_159), Some(12_288)), Some(6_400));
        assert_eq!(cap(Some(20_000), Some(12_288)), Some(12_288));
        assert_eq!(cap(None, Some(12_288)), Some(12_288));
        // A run's cap below the floor: the run's cap, never a panic.
        assert_eq!(cap(Some(100), Some(512)), Some(512));
        assert_eq!(cap(Some(1_000), None), None);
        assert_eq!(
            ServiceMemory::sized(Some(4_159), Some(12_288)).reservation_mb,
            7_424
        );
        assert_eq!(ServiceMemory::sized(Some(100), None).reservation_mb, 5_120);
    }

    #[test]
    fn the_width_line_says_the_budget_and_the_caps() {
        let run_cap = Some(12_288);
        let memories = [
            ServiceMemory::sized(Some(400), run_cap),
            ServiceMemory::sized(None, run_cap),
            ServiceMemory::sized(Some(9_000), run_cap),
        ];
        assert_eq!(
            plan_under(16_384, 1_700, 4, None).line(&memories, run_cap),
            "Analysing services by memory: budget 12636 MB (limit from env), at most 3 at once; \
             2 with a stored peak, 2 at the full cap"
        );
        assert_eq!(
            plan_under(16_384, 1_700, 4, Some(2)).line(&memories, run_cap),
            "Analysing services by memory: at most 2 at once: asked for by \
             CARRICK_SERVICE_CONCURRENCY; the budget is not read; 2 with a stored peak, 2 at the \
             full cap"
        );
    }

    /// The services of the first index of 43 the setups-ahead queue was
    /// written from (`setup_ahead`'s `LOGGED`), each taking what its read and
    /// analysis took once its setup was asked ahead, replayed through the
    /// admission at widths 1 to 8.
    ///
    /// A projection, not a measure: it holds each service's time fixed, where
    /// side by side they share the process-wide request cap and the gateway's
    /// rate. What it does show is the floor the admission leaves: the pass can
    /// end no earlier than its longest service.
    #[tokio::test(start_paused = true)]
    async fn a_large_first_index_projected_at_widths_one_to_eight() {
        let took: Vec<Duration> = crate::engine::setup_ahead::tests::logged_analysis_times();
        assert_eq!(took.len(), 43);
        let longest = *took.iter().max().unwrap();
        let mut projected = Vec::new();
        for width in 1..=8 {
            let started = tokio::time::Instant::now();
            let plan = Plan::count(width, "the projection's");
            in_declared_order(&Ledger::new(), &plan, &vec![0; took.len()], |index| {
                tokio::time::sleep(took[index])
            })
            .await;
            projected.push(started.elapsed());
        }
        take_schedules();
        let total: Duration = took.iter().sum();
        assert_eq!(projected[0], total, "one at a time is the sum");
        assert!(projected.windows(2).all(|pair| pair[1] <= pair[0]));
        assert!(projected[7] >= longest);
        let minutes: Vec<String> = projected
            .iter()
            .enumerate()
            .map(|(at, took)| format!("{}: {:.1} min", at + 1, took.as_secs_f64() / 60.0))
            .collect();
        eprintln!(
            "projected pass by width ({:.1} min longest service): {}",
            longest.as_secs_f64() / 60.0,
            minutes.join(", ")
        );
    }

    /// A job's thread drives its future on the scan's runtime, and a panic
    /// in it reaches the coordinator.
    #[tokio::test]
    async fn a_thread_drives_its_future_and_a_panic_reaches_the_coordinator() {
        let answer = on_a_thread("test-service".to_string(), |handle| {
            handle.block_on(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                std::thread::current().name().map(str::to_string)
            })
        })
        .await;
        assert_eq!(answer.as_deref(), Some("test-service"));

        let raised =
            futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(on_a_thread::<(), _>(
                "test-panic".to_string(),
                |_| panic!("the service broke"),
            )))
            .await;
        let payload = raised.expect_err("the panic is raised again");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"the service broke"));
    }
}
