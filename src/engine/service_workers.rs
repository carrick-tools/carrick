//! A scan's services analysed side by side, each on a thread of its own
//! (carrick#1066).
//!
//! Services used to be analysed one after another, so a large first index
//! kept one or two file analyses in flight against a process-wide cap many
//! times that, and each service's local work (its sidecar starting, discovery,
//! signatures, capture) was time with no request outstanding.
//!
//! - [`width`] decides how many run at once, from the machine and
//!   [`SERVICE_CONCURRENCY_ENV`].
//! - [`in_declared_order`] admits services in declared order as a slot frees.
//!   It knows nothing of threads, so a replay on a paused clock drives the
//!   same admission the scan uses.
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

/// The stack each service's thread runs on: the main thread's, which the
/// analysis ran on before it had threads of its own. A spawned thread's
/// default is a quarter of it, and the parser recurses as deep as the source.
const SERVICE_STACK_BYTES: usize = 8 * 1024 * 1024;

const SERVICES: machine::Sizing = machine::Sizing {
    noun: "service(s)",
    knob: SERVICE_CONCURRENCY_ENV,
    cap: MAX_SERVICES_AT_ONCE,
    cores_floor: 2,
    running: 0,
    knob_capped_by_memory: false,
};

/// How many of a pass's `services` are analysed at once, and why.
///
/// A dispatched run analyses one at a time: whether a service builds its
/// prompts for the job or generates its intents depends on whether the run
/// has collected any prompt yet, a run-level fact that services side by side
/// would race for.
pub fn width(services: usize, dispatching: bool) -> machine::Size {
    let forced = std::env::var(SERVICE_CONCURRENCY_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|n| *n > 0);
    width_for(
        services,
        dispatching,
        machine::available_memory_mb(),
        machine::cores(),
        crate::services::type_sidecar::heap_cap_mb(),
        forced,
    )
}

/// [`width`] with its inputs given: min(services, [`MAX_SERVICES_AT_ONCE`],
/// max(2, cores - 1), the services the free memory holds after
/// [`machine::HEADROOM_MB`] at the heap cap and [`SERVICE_OVERHEAD_MB`] each).
/// `forced` replaces everything but the service count.
fn width_for(
    services: usize,
    dispatching: bool,
    available_mb: Option<u64>,
    cores: usize,
    heap_cap_mb: Option<u64>,
    forced: Option<usize>,
) -> machine::Size {
    if dispatching {
        return machine::Size {
            count: 1,
            why: "1 service(s): a dispatched run analyses one at a time".to_string(),
        };
    }
    let per_mb = heap_cap_mb.unwrap_or(V8_DEFAULT_HEAP_MB) + SERVICE_OVERHEAD_MB;
    let size = machine::size_for(&SERVICES, available_mb, per_mb, cores, forced);
    let count = size.count.min(services.max(1));
    if count == size.count {
        return size;
    }
    machine::Size {
        count,
        why: format!("{count} service(s): the pass has {services}; {}", size.why),
    }
}

/// [`width_for`]'s count for a non-dispatched pass, for a test elsewhere in
/// the crate that sets a sidecar heap cap and reads what width it buys.
#[cfg(test)]
pub(crate) fn width_with(
    services: usize,
    available_mb: Option<u64>,
    cores: usize,
    heap_cap_mb: Option<u64>,
) -> usize {
    width_for(services, false, available_mb, cores, heap_cap_mb, None).count
}

/// How one pass of services was scheduled, for a test to read: proof that
/// services really ran side by side and finished out of order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
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

/// Run `count` jobs, at most `width` at once, each started in declared order
/// as a slot frees, and return their results by index.
///
/// The admission and nothing else: `start` decides where a job runs.
pub async fn in_declared_order<R, Fut>(
    count: usize,
    width: usize,
    start: impl FnMut(usize) -> Fut,
) -> Vec<R>
where
    Fut: Future<Output = R>,
{
    let (results, schedule) = admit(count, width, start).await;
    if let Ok(mut passes) = SCHEDULES.lock() {
        passes.push(schedule);
    }
    results
}

/// [`in_declared_order`] for work that is not a pass of the analysis, such
/// as the walk lane: its schedule is not one [`take_schedules`] returns.
pub(super) async fn admit_in_declared_order<R, Fut>(
    count: usize,
    width: usize,
    start: impl FnMut(usize) -> Fut,
) -> Vec<R>
where
    Fut: Future<Output = R>,
{
    admit(count, width, start).await.0
}

async fn admit<R, Fut>(
    count: usize,
    width: usize,
    mut start: impl FnMut(usize) -> Fut,
) -> (Vec<R>, Schedule)
where
    Fut: Future<Output = R>,
{
    let width = width.max(1);
    let mut results: Vec<Option<R>> = (0..count).map(|_| None).collect();
    let mut schedule = Schedule {
        width,
        ..Schedule::default()
    };
    let mut running = FuturesUnordered::new();
    let mut next = 0;
    loop {
        while next < count && running.len() < width {
            let index = next;
            next += 1;
            let job = start(index);
            running.push(async move { (index, job.await) });
            schedule.most_in_flight = schedule.most_in_flight.max(running.len());
        }
        let Some((index, result)) = running.next().await else {
            break;
        };
        schedule.finished.push(index);
        results[index] = Some(result);
    }
    let results = results
        .into_iter()
        .map(|result| result.expect("every admitted job ends with a result"))
        .collect();
    (results, schedule)
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

    /// The width rule, one row per input that decides it.
    #[test]
    fn the_width_is_the_least_of_services_cap_cores_and_memory() {
        let width = |services, available: Option<u64>, cores, heap: Option<u64>, forced| {
            width_for(services, false, available, cores, heap, forced).count
        };
        // 32 GB free, 16 cores, the 8 GB heap: (32 - 2) / 9 holds 3.
        assert_eq!(width(10, Some(32 * GB), 16, Some(8 * GB), None), 3);
        // A laptop with 12 GB free at the 8 GB heap: one.
        assert_eq!(width(10, Some(12 * GB), 10, Some(8 * GB), None), 1);
        // A runner with 64 GB and a 3 GB heap per shape: cores decide.
        assert_eq!(width(10, Some(64 * GB), 4, Some(3 * GB), None), 3);
        // Two cores still run two: the floor.
        assert_eq!(width(10, Some(64 * GB), 2, Some(3 * GB), None), 2);
        assert_eq!(width(10, Some(64 * GB), 1, Some(3 * GB), None), 2);
        // Never more than eight, nor more than the pass has.
        assert_eq!(width(20, Some(512 * GB), 64, Some(2 * GB), None), 8);
        assert_eq!(width(3, Some(512 * GB), 64, Some(2 * GB), None), 3);
        assert_eq!(width(1, Some(512 * GB), 64, Some(2 * GB), None), 1);
        // Too little memory for one: one, never zero.
        assert_eq!(width(10, Some(GB), 16, Some(8 * GB), None), 1);
        // Memory that cannot be read: one.
        assert_eq!(width(10, None, 16, Some(8 * GB), None), 1);
        // The heap cap turned off: V8's own default is the size.
        assert_eq!(width(10, Some(17 * GB), 16, None, None), 3);
        // The knob is honoured above the memory bound, and below it.
        assert_eq!(width(10, Some(12 * GB), 10, Some(8 * GB), Some(4)), 4);
        assert_eq!(width(10, None, 16, Some(8 * GB), Some(4)), 4);
        assert_eq!(width(10, Some(512 * GB), 64, Some(2 * GB), Some(1)), 1);
        // But not above the services the pass has.
        assert_eq!(width(2, Some(12 * GB), 10, Some(8 * GB), Some(4)), 2);
        // A dispatched run: one, whatever is asked.
        assert_eq!(
            width_for(10, true, Some(512 * GB), 64, Some(2 * GB), Some(4)).count,
            1
        );
    }

    #[test]
    fn the_width_says_why() {
        let why = width_for(10, false, Some(32 * GB), 16, Some(8 * GB), None).why;
        assert_eq!(
            why,
            "3 service(s): 8 allowed by 16 core(s) and 32768 MB free holds 3 at 9216 MB each \
             after 2048 MB kept back"
        );
        let why = width_for(2, false, Some(32 * GB), 16, Some(8 * GB), Some(4)).why;
        assert!(
            why.starts_with("2 service(s): the pass has 2; 4 service(s)"),
            "{why}"
        );
    }

    /// Jobs start in declared order, no more than the width at once, and
    /// each result is kept at its own index whatever order they end in.
    #[tokio::test(start_paused = true)]
    async fn jobs_start_in_declared_order_and_keep_their_index() {
        take_schedules();
        let took = [5u64, 1, 2, 1];
        let started = std::cell::RefCell::new(Vec::new());
        let results = in_declared_order(took.len(), 2, |index| {
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
            in_declared_order(took.len(), width, |index| tokio::time::sleep(took[index])).await;
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
