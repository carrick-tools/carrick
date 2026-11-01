//! A service's file-reader answers, asked while the scan analyses the service
//! before it (carrick#3131).
//!
//! A pull request scan asks the file reader about each service's changed
//! files inside that service's analysis, and the service's type work waits
//! for every answer. Nothing was asked for the next service meanwhile: at one
//! service at a time the reader sat idle while a service spent minutes on its
//! types, and then the next service waited for its own answers.
//!
//! The lane here asks them ahead. It takes the analysed services in declared
//! order, each once the analysis before it has started and its own gate is
//! open (the walks it reads have ended, carrick#3085), and builds the
//! service's prompts as its analysis will build them: the same discovery, the
//! same previous generation, the same setup and the same first answer from
//! the library store. It asks them and holds each answer under the bytes it
//! asked ([`crate::agents::asked_ahead`]). The analysis takes an answer only
//! for the bytes it built itself, and asks anything else as it always did.
//!
//! What it keeps as it was, and what bounds it:
//!
//! * **Nothing is asked twice.** The analysis and the lane meet at one slot
//!   per service. An analysis that claims it before the lane has published
//!   asks for itself, and the lane then asks nothing for that service. The
//!   library store is asked once per service as well: by the lane, which
//!   hands its first answer and its settled answer to the analysis
//!   ([`Place::begin_library`], [`Place::claim_library`]), or by the analysis.
//! * **It reads what it uses without taking it.** A setup is the one the
//!   previous generation replays, or the one the setup queue asked, read from
//!   it ([`super::setup_ahead::SetupsAhead::ready`]); the analysis takes it
//!   from its slot as before. The lane asks no setup and no client semantics.
//! * **It changes no admission.** It reserves nothing from the run's memory
//!   budget, starts no type checker, and only hears when a gate opens and an
//!   analysis starts ([`super::service_workers::WalkedPass::gate_opened`]).
//! * **Bounded.** At most one service ahead of the scan, at most a queue's
//!   depth of asks in flight, every request inside the process-wide cap, and
//!   nothing asked once the run's retry budget is spent.
//! * **Off** when [`ENV`] is `0`, and in a dispatched run, a resumed run, a
//!   run with no model, for a service with no previous generation of this
//!   cache version (a first index), and in the retry of owed work.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::FutureExt;
use futures::future::Shared;
use tokio::sync::{oneshot, watch};

use crate::agents::asked_ahead::AheadSlot;
use crate::library_store::{Asked, FirstAnswer, Settled};

/// Set to `0` to ask nothing ahead: each service's files are asked when its
/// analysis reaches them. Documented for runners in
/// `docs/reference/runner.md`.
pub const ENV: &str = "CARRICK_ANSWERS_AHEAD";

/// Whether this run asks the file reader ahead.
pub(super) fn requested() -> bool {
    std::env::var(ENV).ok().as_deref().map(str::trim) != Some("0")
        && !crate::analysis_channel::dispatching()
        && crate::analysis_channel::answers().is_none()
        && !crate::local_mode::no_model()
}

/// The library store's answers for one service, asked by the lane and handed
/// to its analysis.
pub(super) struct LibraryHanded {
    /// What was asked: the analysis takes the answers only for the same ask.
    pub(super) asked: Asked,
    first: Shared<oneshot::Receiver<FirstAnswer>>,
    settled: tokio::task::JoinHandle<Settled>,
}

impl LibraryHanded {
    /// The answers of an ask already started: `first` heard when the store
    /// first answers, `settled` once it stops asking.
    pub(super) fn new(
        asked: Asked,
        first: oneshot::Receiver<FirstAnswer>,
        settled: tokio::task::JoinHandle<Settled>,
    ) -> Self {
        Self {
            asked,
            first: first.shared(),
            settled,
        }
    }

    /// The store's first answer, for the lane's own prompts. `None` when it
    /// never came.
    pub(super) fn first(&self) -> impl Future<Output = Option<FirstAnswer>> + Send + use<> {
        self.first.clone().map(Result::ok)
    }

    /// The settled answer, with the first sent on `early` the moment it is
    /// in hand: what [`super::settled_library_answer`] gives the analysis
    /// when it asks for itself.
    pub(super) async fn settle(self, early: oneshot::Sender<FirstAnswer>) -> Settled {
        if let Ok(first) = self.first.await {
            let _ = early.send(first);
        }
        self.settled.await.unwrap_or_default()
    }
}

/// Where the library store's ask for one service stands.
enum LibraryState {
    /// Neither the lane nor the analysis has come.
    Open,
    /// The lane came first, and asked this (`None`: the service asks the
    /// store nothing).
    Lane(Option<LibraryHanded>),
    /// The analysis came.
    Claimed,
}

/// What the lane and one service's analysis share.
pub(super) struct Place {
    started: watch::Sender<bool>,
    gate: watch::Sender<bool>,
    /// The file reader's answers asked ahead.
    pub(super) answers: Arc<AheadSlot>,
    library: Mutex<LibraryState>,
}

impl Place {
    fn new() -> Self {
        Self {
            started: watch::Sender::new(false),
            gate: watch::Sender::new(false),
            answers: Arc::new(AheadSlot::default()),
            library: Mutex::new(LibraryState::Open),
        }
    }

    /// The lane starts the service's library ask with `start`, unless the
    /// analysis came first. `false` then: the lane asks nothing for the
    /// service. `start` runs under the place's lock, so the analysis sees
    /// either no ask or the one started.
    pub(super) fn begin_library(&self, start: impl FnOnce() -> Option<LibraryHanded>) -> bool {
        let mut state = self.library.lock().unwrap_or_else(PoisonError::into_inner);
        match *state {
            LibraryState::Open => {
                *state = LibraryState::Lane(start());
                true
            }
            _ => false,
        }
    }

    /// What the lane asked the library store, taken once by the analysis.
    /// Closes the place either way.
    pub(super) fn claim_library(&self) -> Option<LibraryHanded> {
        let mut state = self.library.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, LibraryState::Claimed) {
            LibraryState::Lane(handed) => handed,
            _ => None,
        }
    }

    /// Whether the service's analysis has started.
    pub(super) fn has_started(&self) -> bool {
        *self.started.borrow()
    }

    /// Resolves once the service's analysis has started.
    pub(super) fn started(&self) -> impl Future<Output = ()> + Send + use<> {
        let mut started = self.started.subscribe();
        async move {
            let _ = started.wait_for(|started| *started).await;
        }
    }
}

/// The lane's and the loop's shared view of one pass's analysed services.
pub(super) struct Board {
    places: Vec<Place>,
    /// Set when the scan no longer wants anything asked ahead.
    pub(super) stop: Arc<AtomicBool>,
    /// The same, for a wait to hear.
    stopped: watch::Sender<bool>,
}

impl Board {
    /// One place per analysed service. `gated` is whether each analysis waits
    /// for walks: without, every gate is open from the start.
    pub(super) fn new(services: usize, gated: bool) -> Self {
        let places: Vec<Place> = (0..services).map(|_| Place::new()).collect();
        if !gated {
            for place in &places {
                place.gate.send_replace(true);
            }
        }
        Self {
            places,
            stop: Arc::new(AtomicBool::new(false)),
            stopped: watch::Sender::new(false),
        }
    }

    /// Ask nothing more ahead.
    fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.stopped.send_replace(true);
    }

    /// Whether the scan no longer wants anything asked ahead.
    pub(super) fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Resolves once the scan no longer wants anything asked ahead.
    pub(super) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        let mut stopped = self.stopped.subscribe();
        async move {
            let _ = stopped.wait_for(|stopped| *stopped).await;
        }
    }

    pub(super) fn len(&self) -> usize {
        self.places.len()
    }

    pub(super) fn place(&self, at: usize) -> Option<&Place> {
        self.places.get(at)
    }

    /// The analysis at `at` has started.
    pub(super) fn mark_started(&self, at: usize) {
        if let Some(place) = self.places.get(at) {
            place.started.send_replace(true);
        }
    }

    /// Every walk the analysis at `at` reads has ended.
    pub(super) fn mark_gate_open(&self, at: usize) {
        if let Some(place) = self.places.get(at) {
            place.gate.send_replace(true);
        }
    }
}

/// The lane: for each analysed service after the first, in order, wait until
/// the analysis before it has started and its own gate is open, then run
/// `ask` for it to its end. A service whose analysis started first is
/// skipped: it asks for itself. Returns once every service has been passed.
pub(super) async fn run<Ask, AskFut>(board: Arc<Board>, mut ask: Ask)
where
    Ask: FnMut(usize) -> AskFut,
    AskFut: Future<Output = ()>,
{
    for at in 1..board.len() {
        board.places[at - 1].started().await;
        let place = &board.places[at];
        let mut gate = place.gate.subscribe();
        tokio::select! {
            biased;
            _ = place.started() => continue,
            _ = gate.wait_for(|open| *open) => {}
        }
        if place.has_started() || board.is_stopped() {
            continue;
        }
        ask(at).await;
    }
}

/// The lane's task, stopped when dropped: nothing more is asked ahead.
pub(super) struct LaneTask {
    task: tokio::task::JoinHandle<()>,
    board: Arc<Board>,
}

impl LaneTask {
    pub(super) fn spawn(
        board: &Arc<Board>,
        lane: impl Future<Output = ()> + Send + 'static,
    ) -> Self {
        Self {
            task: tokio::spawn(lane),
            board: board.clone(),
        }
    }
}

impl Drop for LaneTask {
    fn drop(&mut self) {
        self.board.halt();
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::asked_ahead::AskedAhead;
    use std::time::Duration;
    use tokio::time::{Instant, sleep, sleep_until};

    fn tenths(n: u64) -> Duration {
        Duration::from_millis(n * 100)
    }

    /// One service of a logged pull request scan of the pilot repository (38
    /// services, one at a time, 0.3.134), in tenths of a second: its
    /// discovery, its `model` phase, the rest of its analysis, whether it
    /// asked the file reader anything, and its `model` floor (the least that
    /// phase took on any scan of it at current pins, so with no reader ask).
    type Logged = (u64, u64, u64, bool, u64);

    const LOGGED: [Logged; 38] = [
        (70, 14, 142, false, 13),
        (33, 137, 1923, true, 29),
        (17, 7, 23, false, 5),
        (17, 19, 949, false, 16),
        (38, 200, 1924, true, 41),
        (49, 627, 2079, true, 39),
        (24, 7, 25, false, 4),
        (23, 24, 58, true, 12),
        (19, 66, 80, true, 8),
        (18, 6, 53, false, 5),
        (19, 4, 126, false, 4),
        (18, 3, 17, false, 3),
        (24, 2, 24, false, 2),
        (17, 7, 49, false, 6),
        (20, 9, 88, false, 6),
        (23, 5, 69, false, 5),
        (20, 5, 112, false, 5),
        (20, 7, 131, false, 7),
        (20, 6, 104, false, 5),
        (20, 6, 52, false, 5),
        (21, 6, 55, false, 5),
        (17, 6, 55, false, 4),
        (21, 6, 32, false, 6),
        (22, 6, 132, false, 5),
        (20, 2, 113, false, 2),
        (17, 7, 142, false, 5),
        (18, 2, 24, false, 2),
        (20, 5, 31, false, 4),
        (20, 6, 26, false, 4),
        (21, 5, 25, false, 4),
        (18, 42, 134, false, 41),
        (21, 5, 211, false, 4),
        (17, 5, 204, false, 4),
        (19, 5, 235, false, 4),
        (20, 5, 211, false, 4),
        (20, 5, 70, false, 5),
        (19, 5, 143, false, 4),
        (17, 4, 130, false, 4),
    ];

    /// What a replay of [`LOGGED`] came to.
    struct Replayed {
        /// How long each service's `model` phase waited for answers beyond
        /// its floor.
        waited: Vec<Duration>,
        pass_took: Duration,
        /// Services whose answers were asked once, by the lane or by the
        /// analysis, and asked at all.
        asked_once: usize,
        asked_twice: usize,
    }

    /// [`LOGGED`] analysed one at a time, with the lane asking ahead or not.
    /// The lane's work for a service is its discovery, then its asks, which
    /// take what its `model` phase took beyond its floor. The analysis claims
    /// the service's slot after its own discovery: answers published there
    /// are waited for, and its `model` phase is then its floor; otherwise it
    /// asks for itself and the phase takes what it took. The walks' ends are
    /// not in the log, so every gate is open from the start.
    async fn replay(lane_on: bool) -> Replayed {
        let board = Arc::new(Board::new(LOGGED.len(), false));
        let slots: Arc<Vec<Arc<AheadSlot>>> = Arc::new(
            (0..LOGGED.len())
                .map(|at| board.place(at).unwrap().answers.clone())
                .collect(),
        );
        // When the lane's asks for each service end.
        let asks_end: Arc<Vec<watch::Sender<Option<Instant>>>> = Arc::new(
            (0..LOGGED.len())
                .map(|_| watch::Sender::new(None))
                .collect(),
        );
        let lane_asked = Arc::new(Mutex::new(vec![0usize; LOGGED.len()]));
        let _lane = lane_on.then(|| {
            let (board, slots, asks_end, lane_asked) = (
                board.clone(),
                slots.clone(),
                asks_end.clone(),
                lane_asked.clone(),
            );
            LaneTask::spawn(
                &board.clone(),
                run(board, move |at| {
                    let (slots, asks_end, lane_asked) =
                        (slots.clone(), asks_end.clone(), lane_asked.clone());
                    async move {
                        let (discover, model, _, asks, floor) = LOGGED[at];
                        sleep(tenths(discover)).await;
                        if !asks || !slots[at].publish(Arc::new(AskedAhead::default())) {
                            return;
                        }
                        lane_asked.lock().unwrap()[at] += 1;
                        sleep(tenths(model - floor)).await;
                        asks_end[at].send_replace(Some(Instant::now()));
                    }
                }),
            )
        });
        let start = Instant::now();
        let mut waited = Vec::new();
        let mut loop_asked = vec![0usize; LOGGED.len()];
        for (at, (discover, model, other, asks, floor)) in LOGGED.into_iter().enumerate() {
            board.mark_started(at);
            sleep(tenths(discover)).await;
            if slots[at].claim().is_some() {
                let mut end = asks_end[at].subscribe();
                let phase = Instant::now();
                let ended = *end.wait_for(Option::is_some).await.unwrap();
                let ended = ended.unwrap();
                waited.push(ended.saturating_duration_since(phase));
                sleep_until(ended.max(phase)).await;
                sleep(tenths(floor)).await;
            } else {
                if asks {
                    loop_asked[at] += 1;
                    waited.push(tenths(model - floor));
                } else {
                    waited.push(Duration::ZERO);
                }
                sleep(tenths(model)).await;
            }
            sleep(tenths(other)).await;
        }
        let pass_took = start.elapsed();
        let lane_asked = lane_asked.lock().unwrap().clone();
        let per_service: Vec<usize> = lane_asked
            .iter()
            .zip(&loop_asked)
            .map(|(lane, own)| lane + own)
            .collect();
        Replayed {
            waited,
            pass_took,
            asked_once: per_service.iter().filter(|asked| **asked == 1).count(),
            asked_twice: per_service.iter().filter(|asked| **asked > 1).count(),
        }
    }

    /// The logged scan replayed: with the lane, the analysis waits for the
    /// reader at no service after the first, and the pass is shorter by
    /// every such service's `model` phase beyond its floor. The reader is
    /// asked about the same services, once each.
    #[tokio::test(start_paused = true)]
    async fn a_logged_pr_scan_waits_for_no_answer_after_its_first_service() {
        let without = replay(false).await;
        let with = replay(true).await;

        let asking: usize = LOGGED.iter().filter(|(.., asks, _)| *asks).count();
        assert_eq!(asking, 5);
        assert_eq!((without.asked_once, without.asked_twice), (asking, 0));
        assert_eq!(
            (with.asked_once, with.asked_twice),
            (asking, 0),
            "every service that asks is asked once"
        );
        assert_eq!(
            with.waited[1..],
            vec![Duration::ZERO; LOGGED.len() - 1],
            "the analysis waited for the reader after the first service"
        );
        let hidden: Duration = LOGGED[1..]
            .iter()
            .filter(|(.., asks, _)| *asks)
            .map(|(_, model, _, _, floor)| tenths(model - floor))
            .sum();
        assert_eq!(
            hidden,
            tenths(925),
            "92.5 s of reader waits after the first"
        );
        assert_eq!(without.pass_took - with.pass_took, hidden);
        eprintln!(
            "replay: the pass took {:.1}s without the lane and {:.1}s with it",
            without.pass_took.as_secs_f64(),
            with.pass_took.as_secs_f64()
        );
    }

    /// The lane waits for the analysis before a service to start, and skips a
    /// service whose own analysis started first.
    #[tokio::test(start_paused = true)]
    async fn the_lane_is_one_service_ahead_and_skips_one_already_started() {
        let board = Arc::new(Board::new(3, false));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let lane = {
            let asked = asked.clone();
            LaneTask::spawn(
                &board.clone(),
                run(board.clone(), move |at| {
                    let asked = asked.clone();
                    async move {
                        asked.lock().unwrap().push(at);
                    }
                }),
            )
        };
        sleep(tenths(1)).await;
        assert!(asked.lock().unwrap().is_empty(), "nothing before the first");
        board.mark_started(0);
        sleep(tenths(1)).await;
        assert_eq!(*asked.lock().unwrap(), vec![1]);
        // The third service starts before the second does: the lane does
        // not ask for it.
        board.mark_started(2);
        board.mark_started(1);
        sleep(tenths(1)).await;
        assert_eq!(*asked.lock().unwrap(), vec![1]);
        drop(lane);
    }

    /// A service's gate holds the lane until its walks have ended.
    #[tokio::test(start_paused = true)]
    async fn the_lane_waits_for_a_service_s_gate() {
        let board = Arc::new(Board::new(2, true));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let _lane = {
            let asked = asked.clone();
            LaneTask::spawn(
                &board.clone(),
                run(board.clone(), move |at| {
                    let asked = asked.clone();
                    async move {
                        asked.lock().unwrap().push(at);
                    }
                }),
            )
        };
        board.mark_started(0);
        sleep(tenths(1)).await;
        assert!(asked.lock().unwrap().is_empty(), "its walks have not ended");
        board.mark_gate_open(1);
        sleep(tenths(1)).await;
        assert_eq!(*asked.lock().unwrap(), vec![1]);
    }

    /// The library store is asked once: the place goes to whichever of the
    /// lane and the analysis comes first.
    #[test]
    fn the_library_ask_goes_to_whoever_comes_first() {
        let place = Place::new();
        assert!(place.claim_library().is_none());
        let mut started = false;
        assert!(!place.begin_library(|| {
            started = true;
            None
        }));
        assert!(!started, "the lane starts nothing once the analysis came");

        let place = Place::new();
        assert!(place.begin_library(|| None));
        assert!(place.claim_library().is_none(), "the lane asked nothing");
        assert!(!place.begin_library(|| None), "and the place is claimed");
    }
}
