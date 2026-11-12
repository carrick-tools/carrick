//! Where a service's analysis phase spent its wall clock.
//!
//! The per-service line already stated `packages`, `sidecar` and `analysis`,
//! and `analysis` was everything: on a warm rescan of a 33-service monorepo
//! that one number was 774 s in one release and 1,024 s in the next with the
//! same tree, and nothing in the log said which part of it grew (carrick#767).
//! This records the elapsed time of each stage inside that phase so the line
//! attributes itself.
//!
//! The recorder belongs to the service's analysis future, a task-local, for
//! the same reason [`crate::current_service`] does: the stages sit in two
//! branches (incremental and full) several call layers below the loop that
//! prints the line, and a return value would have to be carried through every
//! one of them. Two services analysed side by side each have a recorder of
//! their own, so a run's marks belong to exactly one service (carrick#1066).
//!
//! The rendered line is a contract with the rescan gate, which parses
//! `analysis <seconds>s` per service and refuses a run whose total has grown
//! against the previous scan of the same tree. Keep the field order and the
//! `name 0.0s` shape stable.
//!
//! The same marks are also the scan's local progress, on the log's own channel
//! (stderr, at info): a line when a service's analysis starts, a line as each
//! stage ends, and a heartbeat every [`HEARTBEAT`] while a stage runs longer
//! than that. A scan that went silent after its setup calls for 2 h 50 min
//! until the cloud killed it, and nothing said which local stage it was in
//! (carrick#3896). A stage's start is the end of the one before it, which is
//! why a heartbeat names the stage that last ended: the one running is the one
//! after it. Every line carries the stage name, the service's position, the
//! elapsed time and the process's resident memory, and never a path, a name
//! or code.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a stage may run before the log says it is still running, and how
/// often it says so after that.
pub const HEARTBEAT: Duration = Duration::from_secs(60);

/// The stages of one service's analysis, in the order the line prints them.
///
/// Every stage is printed on every service, including the ones that cost
/// nothing: a phase missing from the line reads as "not measured", and the
/// point of the line is that the seconds add up to `analysis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// File discovery and the SWC pass over them (parse).
    Discover,
    /// Reading the previous scan's answers and deciding what is reusable.
    Cache,
    /// The compiler engine's `implementations` ask: the interface members the
    /// service's calls pass through, read once before the walks (carrick#3861).
    Implementations,
    /// The file-analyzer stage: deterministic emission, the model calls this
    /// scan dispatches, and the join of the two.
    Model,
    /// Mount-graph construction.
    Graph,
    /// The deterministic protocol scans (GraphQL, sockets, pub/sub).
    Protocols,
    /// The wait for function intents left once the stages before it are done.
    /// Intents start at discovery and run beside `Model`, `Graph` and
    /// `Protocols` (carrick#1065), so most of their wall clock is inside
    /// those marks; a small figure here does not mean intents did not run.
    Intents,
    /// Function-signature composition, including sidecar inference.
    Signatures,
    /// Assembling the payload from the graph and the two whole-workspace
    /// passes over it: external call candidates and the SDK surface.
    Surface,
    /// Type-manifest assembly and the collection of the protocol type
    /// requests.
    Manifest,
    /// Sidecar type resolution: the inference and bundle round trips, and the
    /// scanner-side collection of the requests they carry.
    Types,
    /// The v2 capture round trip, the literal backfill included (carrick#2041).
    Capture,
    /// Per-endpoint definition resolution from the capture stub.
    Definitions,
    /// Everything else between the marks above.
    Other,
}

impl Phase {
    const ORDER: [Phase; 14] = [
        Phase::Discover,
        Phase::Cache,
        Phase::Implementations,
        Phase::Model,
        Phase::Graph,
        Phase::Protocols,
        Phase::Intents,
        Phase::Signatures,
        Phase::Surface,
        Phase::Manifest,
        Phase::Types,
        Phase::Capture,
        Phase::Definitions,
        Phase::Other,
    ];

    /// Whether this stage is time spent waiting on the model.
    ///
    /// The two stages that ask it, and no others: the file-analyzer calls and
    /// the function intents. Everything else is work this machine does, which
    /// is the split the build states before and after the wait
    /// (carrick#1452).
    fn is_model(self) -> bool {
        matches!(self, Phase::Model | Phase::Intents)
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Discover => "discover",
            Phase::Cache => "cache",
            Phase::Implementations => "implementations",
            Phase::Model => "model",
            Phase::Graph => "graph",
            Phase::Protocols => "protocols",
            Phase::Intents => "intents",
            Phase::Signatures => "signatures",
            Phase::Surface => "surface",
            Phase::Manifest => "manifest",
            Phase::Types => "types",
            Phase::Capture => "capture",
            Phase::Definitions => "definitions",
            Phase::Other => "other",
        }
    }
}

#[derive(Debug)]
struct Recorder {
    /// When the stage currently being timed started.
    last: Instant,
    totals: BTreeMap<Phase, f64>,
    /// When this service's analysis started.
    started: Instant,
    /// The service's place in the run: its number and how many there are.
    position: (usize, usize),
    /// The stage that last ended; `analysis` before the first has.
    after: &'static str,
}

/// This process's resident memory in MB, as the line states it.
fn rss_text() -> String {
    crate::services::machine::resident_bytes(std::process::id())
        .map_or_else(|| "unknown".to_string(), |bytes| (bytes >> 20).to_string())
}

/// One progress line: what happened, the service's place in the run, the
/// time, and the memory. Nothing in it names a file, a repo or a service.
fn progress_line(what: &str, position: (usize, usize), figures: &str, elapsed: Duration) -> String {
    format!(
        "Phase progress: {what} (service {}/{}, {figures}elapsed {} ms, rss {} MB)",
        position.0,
        position.1,
        elapsed.as_millis(),
        rss_text()
    )
}

/// Say, every `interval` a stage runs on, that it is still running. A thread
/// of its own, not a task: a stage that holds a runtime worker for minutes
/// must not hold the line back, and the line must not wait for the work. It
/// ends when `stop`'s sender is dropped.
fn heartbeat(
    recorder: Arc<Mutex<Recorder>>,
    interval: Duration,
    stop: mpsc::Receiver<()>,
    dispatch: tracing::Dispatch,
) {
    tracing::dispatcher::with_default(&dispatch, || {
        let mut beat = Instant::now();
        loop {
            let since = |recorder: &Recorder| recorder.last.max(beat).elapsed();
            let wait = {
                let recorder = recorder.lock().unwrap_or_else(PoisonError::into_inner);
                interval.saturating_sub(since(&recorder))
            };
            match stop.recv_timeout(wait) {
                Err(RecvTimeoutError::Disconnected) | Ok(()) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let recorder = recorder.lock().unwrap_or_else(PoisonError::into_inner);
            if since(&recorder) >= interval {
                beat = Instant::now();
                tracing::info!(
                    "{}",
                    progress_line(
                        &format!("still running after {}", recorder.after),
                        recorder.position,
                        &format!("{} ms in this stage, ", recorder.last.elapsed().as_millis()),
                        recorder.started.elapsed(),
                    )
                );
            }
        }
    });
}

tokio::task_local! {
    /// The recorder of the service the running task is analysing.
    static RECORDER: Arc<Mutex<Recorder>>;
}

/// Run `work` as one service's analysis and record its stages: every
/// [`mark`] made inside it, on its task, lands here and in no other service's
/// recording. Returns what `work` returned and the stages it spent, even when
/// `work` ended in an error.
///
/// `position` is the service's number and the count of services analysed,
/// for the progress lines.
pub async fn record<Work: Future>(position: (usize, usize), work: Work) -> (Work::Output, Totals) {
    record_with(position, HEARTBEAT, work).await
}

/// [`record`] with its heartbeat interval named, for the test that cannot
/// wait a minute.
async fn record_with<Work: Future>(
    position: (usize, usize),
    interval: Duration,
    work: Work,
) -> (Work::Output, Totals) {
    let now = Instant::now();
    let recorder = Arc::new(Mutex::new(Recorder {
        last: now,
        totals: BTreeMap::new(),
        started: now,
        position,
        after: "analysis",
    }));
    tracing::info!(
        "{}",
        progress_line("analysis started", position, "", Duration::ZERO)
    );
    let (stop, stopped) = mpsc::channel();
    let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
    let beating = recorder.clone();
    // Detached: it ends the moment `stop` is dropped below.
    std::thread::spawn(move || heartbeat(beating, interval, stopped, dispatch));
    let output = RECORDER.scope(recorder.clone(), work).await;
    drop(stop);
    tracing::info!(
        "{}",
        progress_line(
            "analysis ended",
            position,
            "",
            recorder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .started
                .elapsed()
        )
    );
    let recorder = recorder.lock().unwrap_or_else(PoisonError::into_inner);
    (
        output,
        Totals {
            per_phase: recorder.totals.clone(),
        },
    )
}

/// Attribute everything since the previous mark to `phase`.
///
/// A phase may be marked more than once (the incremental branch resolves
/// types, then definitions, then more types on some services); the durations
/// add. A mark made outside [`record`] is dropped.
pub fn mark(phase: Phase) {
    let _ = RECORDER.try_with(|recorder| {
        let mut recorder = recorder.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.duration_since(recorder.last).as_secs_f64();
        *recorder.totals.entry(phase).or_insert(0.0) += elapsed;
        recorder.last = now;
        recorder.after = phase.label();
        tracing::info!(
            "{}",
            progress_line(
                &format!("{} ended", phase.label()),
                recorder.position,
                &format!("took {} ms, ", (elapsed * 1000.0) as u64),
                now.duration_since(recorder.started),
            )
        );
    });
}

/// Run `ask`, attributing the time before it to `before` and the ask itself to
/// [`Phase::Implementations`], so the ask is not counted inside whichever
/// stage's mark follows it (carrick#3861).
pub fn implementations_ask<Answer>(before: Phase, ask: impl FnOnce() -> Answer) -> Answer {
    mark(before);
    let answer = ask();
    mark(Phase::Implementations);
    answer
}

/// One service's recorded stages, read two ways: the line the log states, and
/// the two figures the build's own split is built from.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    per_phase: BTreeMap<Phase, f64>,
}

impl Totals {
    /// The breakdown as the per-service line prints it.
    pub fn line(&self) -> String {
        let parts: Vec<String> = Phase::ORDER
            .iter()
            .map(|phase| {
                format!(
                    "{} {:.1}s",
                    phase.label(),
                    self.per_phase.get(phase).copied().unwrap_or(0.0)
                )
            })
            .collect();
        parts.join(", ")
    }

    /// Seconds spent waiting on the model.
    pub fn model_secs(&self) -> f64 {
        self.sum(true)
    }

    /// Seconds spent on work this machine did.
    pub fn local_secs(&self) -> f64 {
        self.sum(false)
    }

    fn sum(&self, model: bool) -> f64 {
        self.per_phase
            .iter()
            .filter(|(phase, _)| phase.is_model() == model)
            .map(|(_, secs)| secs)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line names every phase in a fixed order whether or not it was
    /// marked, because the gate that parses it counts on the shape; and a
    /// mark outside a recording is dropped, not kept for the next service.
    #[tokio::test]
    async fn line_states_every_phase_in_order_and_only_inside_a_recording() {
        mark(Phase::Cache);
        let ((), totals) = record((1, 1), async {
            mark(Phase::Discover);
        })
        .await;
        let line = totals.line();
        for phase in Phase::ORDER {
            assert!(line.contains(phase.label()), "{line} is missing {phase:?}");
        }
        let discover = line.find("discover").expect("discover present");
        let other = line.find("other").expect("other present");
        assert!(discover < other, "phases print in ORDER: {line}");
        assert!(
            line.contains("cache 0.0s"),
            "an outside mark is dropped: {line}"
        );

        // And the same marks, read as the split the build states: a discover
        // mark is this machine's work, and nothing was spent on the model.
        assert!(totals.local_secs() > 0.0, "discover is local work");
        assert_eq!(totals.model_secs(), 0.0);
    }

    /// The implementations ask has a stage of its own: its time is not in the
    /// stage that follows it, and the line prints it (carrick#3861).
    #[tokio::test]
    async fn implementations_ask_time_is_not_inside_cache() {
        let ((), totals) = record((1, 1), async {
            implementations_ask(Phase::Cache, || {
                std::thread::sleep(std::time::Duration::from_millis(60));
            });
            mark(Phase::Cache);
        })
        .await;
        let secs = |name: &str| -> f64 {
            let line = totals.line();
            let at = line.find(&format!("{name} ")).expect("phase on line") + name.len() + 1;
            line[at..].split('s').next().unwrap().parse().unwrap()
        };
        assert!(secs("implementations") >= 0.05, "{}", totals.line());
        assert!(secs("cache") < 0.05, "{}", totals.line());
    }

    /// Two services analysed side by side, their awaits interleaving, each
    /// take the stages they marked and none of the other's (carrick#1066).
    #[tokio::test]
    async fn services_side_by_side_each_take_only_their_own_stages() {
        let model = record((1, 2), async {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            mark(Phase::Model);
        });
        let types = record((2, 2), async {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            mark(Phase::Types);
        });
        let (((), model), ((), types)) = tokio::join!(model, types);
        assert!(model.model_secs() > 0.0, "the first marked model time");
        assert_eq!(model.local_secs(), 0.0, "and no local stage of its own");
        assert!(types.local_secs() > 0.0, "the second marked a local stage");
        assert_eq!(
            types.model_secs(),
            0.0,
            "and none of the first's model time"
        );
    }

    /// Exactly the two stages that ask the model are model time. A stage
    /// sorted into the wrong half would make the build's split say the wait
    /// was somewhere it was not (carrick#1452).
    #[test]
    fn only_the_stages_that_ask_the_model_are_model_time() {
        let model: Vec<Phase> = Phase::ORDER
            .into_iter()
            .filter(|phase| phase.is_model())
            .collect();
        assert_eq!(model, vec![Phase::Model, Phase::Intents]);
    }

    /// What a subscriber wrote, line by line.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    fn lines_of(captured: &Captured) -> Vec<String> {
        String::from_utf8(captured.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn capture() -> (Captured, tracing::Dispatch) {
        let captured = Captured::default();
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .with_writer(captured.clone())
                .with_ansi(false)
                .finish(),
        );
        (captured, dispatch)
    }

    /// A service's analysis says when it starts, as each stage ends in the
    /// order it ends, and when it ends; each line carries the service's
    /// place, the time and the memory, and no line carries a path.
    #[test]
    fn a_service_says_where_it_is_as_each_stage_ends() {
        let (captured, dispatch) = capture();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            runtime.block_on(record((2, 3), async {
                mark(Phase::Discover);
                mark(Phase::Model);
            }))
        });
        let lines = lines_of(&captured);
        let named: Vec<&str> = [
            "analysis started",
            "discover ended",
            "model ended",
            "analysis ended",
        ]
        .into_iter()
        .collect();
        assert_eq!(lines.len(), named.len(), "{lines:#?}");
        for (line, what) in lines.iter().zip(named) {
            assert!(
                line.contains(&format!("Phase progress: {what} (service 2/3,")),
                "{line}"
            );
            assert!(
                line.contains("elapsed ") && line.contains(" ms") && line.contains("rss "),
                "{line}"
            );
            assert!(
                !line.contains('/') || line.matches('/').count() == 1,
                "no path in {line}"
            );
        }
    }

    /// A stage that runs past the interval is named every interval, by the
    /// stage before it, while it is still running, and the work is not held
    /// back by the heartbeat: a blocking stage still gets its line.
    #[test]
    fn a_long_stage_is_named_while_it_runs() {
        let (captured, dispatch) = capture();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            runtime.block_on(record_with((1, 1), Duration::from_millis(40), async {
                mark(Phase::Discover);
                std::thread::sleep(Duration::from_millis(150));
                mark(Phase::Cache);
            }))
        });
        let lines = lines_of(&captured);
        let beats: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("still running"))
            .collect();
        assert!(beats.len() >= 2, "{lines:#?}");
        for beat in beats {
            assert!(
                beat.contains("still running after discover (service 1/1,"),
                "{beat}"
            );
            assert!(
                beat.contains(" ms in this stage,") && beat.contains("rss "),
                "{beat}"
            );
        }
    }
}
