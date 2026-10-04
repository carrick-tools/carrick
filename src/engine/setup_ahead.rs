//! A service's detection and guidance, asked for before the scan reaches it
//! (carrick#1895).
//!
//! A scan analyses its services one after another. Each one's model setup,
//! its framework detection and then the guidance and extraction config asked
//! from that answer, used to be requested when the loop reached the service.
//! Nothing in a setup waits on another service: detection reads the service's
//! own manifests and imports, and the rest reads detection's answer. So a
//! first index of a repository with many services stood still for each of
//! them in turn, with the model otherwise idle. On one of 43 services that
//! was 22.6 of its 100 minutes, six of them behind a single detection the
//! model was refusing for capacity (`the_setups_of_a_large_first_index_*`
//! below replays its timings).
//!
//! [`SetupsAhead`] asks for them ahead. A queue starts with the scan and works
//! through the services in order on tasks of its own while the loop analyses;
//! when the loop reaches a service its answer is there, or on its way.
//!
//! What the queue keeps as it was, and what bounds it:
//!
//! * **The requests are the ones the loop would make.** A setup is asked by
//!   the function the loop asks with ([`super::ask_model_setup`]), over the
//!   manifests and the import sample the loop reads. The loop takes an answer
//!   asked ahead only when it was asked exactly what the loop would send
//!   ([`ask_identity`]); anything else it asks for itself, as it always did.
//! * **Only what every path asks.** The queue asks where the loop asks
//!   whichever branch it takes ([`StoredSetup::decide`]). A service whose
//!   previous generation holds its setup is replayed by the incremental
//!   branch and is asked nothing here, so a rescan sends what it sent before.
//! * **One ask at a time for one thing asked** ([`AskTurns`]). The cloud keeps
//!   an answer by what was asked and serves the next ask of it from there. Two
//!   services that ask the same thing side by side would both find nothing
//!   kept and both be answered by the model: the spend twice, and two texts
//!   under one guidance id. So the second waits for the first and is then
//!   sent, which is the order the loop asked in.
//! * **Bounded.** At most [`MAX_IN_FLIGHT`] setups are being asked at once,
//!   and one asked ahead starts no sooner than [`START_SPACING`] after the one
//!   before it ([`Bound`]). The setup the scan is waiting for starts at once,
//!   which is when it was asked before there was a queue, so no service waits
//!   longer for its setup than it did. Every request still takes its slot
//!   from the process-wide cap and its route's adaptive limit, and is paced
//!   with every other (`agent_service`).
//! * **It stops when the run's patience is spent.** Detection and guidance
//!   wait out a refusing model on the run's retry budget
//!   ([`crate::retry_budget`]). Once that is spent a setup gets one attempt
//!   and no wait, so the queue asks no further: each remaining service asks
//!   when the loop reaches it, minutes later, as before.
//! * **It stops with the scan.** Dropping [`SetupsAhead`] aborts the queue and
//!   every ask in flight. What an interrupted scan has already asked ahead,
//!   the cloud keeps by what was asked, so the next scan of the same tree is
//!   answered from there.
//!
//! The loop still analyses the services in order, so a setup that is slow
//! holds the loop only at its own service, and only for what is left of a
//! wait that began when the scan did (carrick#1930 is the rest: analysing
//! the services that are ready meanwhile).

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore, oneshot};
use tracing::{debug, info};

use super::durability::ModelSetup;
use super::{CACHE_VERSION, PreviousGeneration, SettledDetection, guidance_is_keyed};
use crate::agents::framework_guidance_agent::ProtocolGuidance;
use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use crate::framework_detector::{DetectionResult, ImportSample};
use crate::packages::Packages;
use crate::services::type_sidecar::ExtractionConfig;

/// Set to how many services' setups are asked for at once, `0` to
/// [`MAX_IN_FLIGHT`]. `0` asks none ahead: every service asks when the loop
/// reaches it.
pub const IN_FLIGHT_ENV: &str = "CARRICK_SETUPS_AHEAD";

/// The most setups asked for at once, and how many are when
/// [`IN_FLIGHT_ENV`] is unset.
///
/// A setup is one detection request, then six on the guidance route at once
/// (five guidance answers and the extraction config). Four setups are at most
/// four requests on the first route and twenty-four on the second: inside
/// the process-wide cap on requests in flight, and no wider on any one route
/// than a service's file analysis already is. It is also enough. A setup
/// takes seconds and a service's analysis takes minutes, so four at a time
/// stay ahead of the loop from its first service on.
const MAX_IN_FLIGHT: usize = 4;

/// The least time between one setup starting and the next one asked ahead
/// of the scan.
///
/// The cloud's gateway meters each route by requests a second, for every
/// caller together, and answers a route that is over its rate before any
/// lambda runs; the scan then slows every route it calls
/// (`agent_service::limiter`). A setup whose answers the cloud already keeps
/// comes back in a fraction of a second, so a queue held only by its width
/// would send a re-run's setups as fast as the cloud could answer them. At
/// this spacing what the queue asks ahead is at most eight requests a second
/// on the guidance route and fewer than two on the detection route, whatever
/// the answers take.
///
/// It does not hold the setup the scan is waiting for: that one starts at
/// once. The scan waits for one service at a time, so those starts come at
/// the pace the loop itself asked at.
const START_SPACING: Duration = Duration::from_millis(750);

/// What bounds the queue: how many setups are asked for at once, and how far
/// apart they start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bound {
    in_flight: usize,
    spacing: Duration,
}

impl Bound {
    /// The run's bound, or `None` when [`IN_FLIGHT_ENV`] asks for none ahead.
    fn of_run() -> Option<Self> {
        Self::read(std::env::var(IN_FLIGHT_ENV).ok().as_deref())
    }

    /// The bound [`IN_FLIGHT_ENV`] set to `in_flight` gives. Anything that is
    /// not a number is the default, and no number raises the maximum.
    fn read(in_flight: Option<&str>) -> Option<Self> {
        let in_flight = in_flight
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(MAX_IN_FLIGHT)
            .min(MAX_IN_FLIGHT);
        (in_flight > 0).then_some(Self {
            in_flight,
            spacing: START_SPACING,
        })
    }
}

/// The part of a service's previous generation its model setup is decided
/// on.
#[derive(Debug, Clone)]
pub(super) struct StoredSetup {
    cache_version: Option<u32>,
    package_json_hash: Option<String>,
    detection: Option<DetectionResult>,
    guidance: Option<ProtocolGuidance>,
    extraction_config: Option<ExtractionConfig>,
}

/// Whether a service's model setup is asked for, and with what.
pub(super) enum SetupAsk {
    /// The previous generation holds the detection and a guidance that can be
    /// replayed, from these manifests: nothing is asked.
    Reuse {
        detection: DetectionResult,
        guidance: ProtocolGuidance,
        extraction_config: Option<ExtractionConfig>,
    },
    /// The setup is asked for. `Some` is a detection the service already
    /// holds, so only what follows from it is asked.
    Ask(Option<SettledDetection>),
}

impl StoredSetup {
    pub(super) fn of(previous: &CloudRepoData) -> Self {
        Self {
            cache_version: previous.cache_version,
            package_json_hash: previous.package_json_hash.clone(),
            detection: previous.cached_detection.clone(),
            guidance: previous.cached_guidance.clone(),
            extraction_config: previous.cached_extraction_config.clone(),
        }
    }

    /// The detection this generation kept when its guidance was deferred
    /// ([`ModelSetup::guidance_deferred`]) or cannot be replayed
    /// ([`guidance_is_keyed`]): there is a cached detection and no usable
    /// cached guidance, from this cache version and these manifests. A
    /// complete generation returns `None`: the incremental branch reuses it
    /// whole, and a full analysis asks again as it always has.
    pub(super) fn kept(&self, package_json_hash: &str) -> Option<SettledDetection> {
        if self.guidance.as_ref().is_some_and(guidance_is_keyed)
            || self.cache_version != Some(CACHE_VERSION)
            || self.package_json_hash.as_deref() != Some(package_json_hash)
        {
            return None;
        }
        self.detection.clone().map(|detection| SettledDetection {
            detection,
            extraction_config: self.extraction_config.clone(),
        })
    }

    /// What the incremental branch does with this generation under the
    /// manifests that hash to `package_json_hash`: replay its setup when it
    /// holds one from this cache version and those manifests, otherwise ask.
    /// The one decision, read by that branch and by the queue.
    ///
    /// A full analysis (no previous generation of this cache version, or a
    /// tree git cannot compare with it) never reaches here and always asks,
    /// with [`Self::kept`]. That is what this returns in every case but
    /// [`SetupAsk::Reuse`], so wherever this says `Ask` both branches send
    /// the same requests, and the queue may send them first. Where it says
    /// `Reuse` the queue asks nothing: if git then cannot compare the tree,
    /// the full analysis asks for itself as before, and the cloud answers
    /// from what it kept.
    pub(super) fn decide(self, package_json_hash: &str) -> SetupAsk {
        let kept = self.kept(package_json_hash);
        let replayable = self.cache_version == Some(CACHE_VERSION)
            && self.package_json_hash.as_deref() == Some(package_json_hash);
        match (self.detection, self.guidance) {
            (Some(detection), Some(guidance)) if replayable && guidance_is_keyed(&guidance) => {
                SetupAsk::Reuse {
                    detection,
                    guidance,
                    extraction_config: self.extraction_config,
                }
            }
            _ => SetupAsk::Ask(kept),
        }
    }
}

/// One ask at a time for each thing asked.
///
/// The cloud serves a detection, a guidance answer and an extraction config
/// from what it kept the last time the same thing was asked, and it has
/// nothing kept until that first ask has answered. While services asked one
/// after another, the second ask of a thing always came after the first. Asked
/// side by side they must still: [`Self::take`] is how.
///
/// One per run, shared by the queue and the loop, so an ask the loop makes
/// itself takes its turn with the ones made ahead.
#[derive(Debug, Clone, Default)]
pub(super) struct AskTurns(Arc<Mutex<HashMap<String, Turn>>>);

/// The turn for one thing asked: held by the ask in flight, queued for by the
/// asks of the same thing behind it.
type Turn = Arc<tokio::sync::Mutex<()>>;

impl AskTurns {
    /// Wait until every earlier ask of `what` has been answered. The turn is
    /// held until the returned guard drops; turns are given in the order they
    /// were asked for.
    pub(super) async fn take(&self, what: String) -> tokio::sync::OwnedMutexGuard<()> {
        let turn = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(what)
            .or_default()
            .clone();
        turn.lock_owned().await
    }
}

/// The names in `list`, as the set the cloud keys an answer on: it sorts and
/// deduplicates each list it is sent, so two orders of one set are one ask.
fn name_set(list: &[String]) -> BTreeSet<&str> {
    list.iter().map(String::as_str).collect()
}

/// What a detection ask is, for [`AskTurns`]: its request body.
pub(super) fn detection_ask(packages: &Packages, import_facts: &ImportSample) -> String {
    let body = crate::framework_detector::request_body(packages, import_facts);
    format!("detection {:x}", Sha256::digest(body.as_bytes()))
}

/// What a service's guidance asks, for [`AskTurns`]: the lists every one of
/// its five requests carries.
pub(super) fn guidance_ask(detection: &DetectionResult) -> String {
    format!(
        "guidance {:?} {:?}",
        name_set(&detection.frameworks),
        name_set(&detection.data_fetchers)
    )
}

/// What an extraction-config ask is, for [`AskTurns`]: the same lists, and
/// the dependency names sent beside them.
pub(super) fn extraction_ask(detection: &DetectionResult, packages: &Packages) -> String {
    let dependencies = packages.cleaned_dependency_names();
    format!(
        "extraction config {:?} {:?} {:?}",
        name_set(&detection.frameworks),
        name_set(&detection.data_fetchers),
        name_set(&dependencies)
    )
}

/// What one model setup asks, as one id: two setups share it only when they
/// send the same requests.
///
/// A setup that asks detection sends the detection body, and everything after
/// it is asked from the answer and the dependency names. One that holds a
/// detection already sends only what follows from that detection.
pub(super) fn ask_identity(
    packages: &Packages,
    import_facts: &ImportSample,
    settled: Option<&SettledDetection>,
) -> String {
    let mut hasher = Sha256::new();
    let mut part = |text: &str| {
        hasher.update((text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    };
    match settled {
        Some(settled) => {
            part("kept");
            part(&serde_json::to_string(&settled.detection).unwrap_or_default());
            part(&serde_json::to_string(&settled.extraction_config).unwrap_or_default());
        }
        None => {
            part("asked");
            part(&crate::framework_detector::request_body(
                packages,
                import_facts,
            ));
        }
    }
    part(&packages.cleaned_dependency_names().join("\n"));
    format!("{:x}", hasher.finalize())
}

/// Why an ask failed, in a form that crosses from the task that asked to the
/// loop that defers the service.
#[derive(Debug)]
pub(super) struct AskFailure(Box<dyn std::error::Error + Send + Sync>);

impl AskFailure {
    /// The call's own error when that is what `error` is, so the deferral
    /// reads its code; otherwise what it says.
    pub(super) fn of(error: Box<dyn std::error::Error>) -> Self {
        match error.downcast::<crate::agent_service::AgentCallError>() {
            Ok(call) => Self(call),
            Err(other) => Self(other.to_string().into()),
        }
    }

    fn as_error(&self) -> &(dyn std::error::Error + 'static) {
        &*self.0
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for AskFailure {
    fn from(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self(error)
    }
}

/// What asking for a service's model setup came to.
///
/// Not yet a [`ModelSetup`], because a failure is said when a setup is made
/// ([`ModelSetup::deferred`] warns), and an answer asked ahead is made a
/// setup when the loop reaches its service, not when it arrives.
#[derive(Debug)]
pub(super) enum SetupAnswer {
    Ready {
        detection: DetectionResult,
        guidance: ProtocolGuidance,
        extraction_config: Option<ExtractionConfig>,
    },
    /// Detection could not be had.
    NoDetection(AskFailure),
    /// Detection answered and guidance could not be had.
    NoGuidance {
        detection: DetectionResult,
        extraction_config: Option<ExtractionConfig>,
        failure: AskFailure,
    },
}

impl SetupAnswer {
    /// The setup the service is analysed under: ready, or deferred as
    /// `super::durability` rules.
    fn into_setup(self) -> ModelSetup {
        match self {
            Self::Ready {
                detection,
                guidance,
                extraction_config,
            } => ModelSetup::ready(detection, guidance, extraction_config),
            Self::NoDetection(failure) => {
                ModelSetup::deferred("framework detection", failure.as_error())
            }
            Self::NoGuidance {
                detection,
                extraction_config,
                failure,
            } => ModelSetup::guidance_deferred(detection, extraction_config, failure.as_error()),
        }
    }
}

/// One service's place in the queue, as the loop holds it: what the queue
/// asked, once it has, and then the answer.
struct Slot<Answer> {
    /// Told when the scan starts waiting for this service's setup.
    wanted: Arc<Notify>,
    identity: oneshot::Receiver<String>,
    answer: oneshot::Receiver<Answer>,
}

/// One service's place in the queue, as the queue holds it.
struct Queued<Service, Answer> {
    service: Service,
    wanted: Arc<Notify>,
    identity: oneshot::Sender<String>,
    answer: oneshot::Sender<Answer>,
}

fn queue_place<Service, Answer>(service: Service) -> (Queued<Service, Answer>, Slot<Answer>) {
    let wanted = Arc::new(Notify::new());
    let (identity, identity_heard) = oneshot::channel();
    let (answer, answer_heard) = oneshot::channel();
    (
        Queued {
            service,
            wanted: wanted.clone(),
            identity,
            answer,
        },
        Slot {
            wanted,
            identity: identity_heard,
            answer: answer_heard,
        },
    )
}

/// What a queue did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Drained {
    /// Setups it asked for.
    asked: usize,
    /// Services it left to the loop because the run's patience was spent.
    left_to_the_loop: usize,
}

/// Work through `queue` in order: read what each service asks (`plan`; `None`
/// is a service that asks nothing ahead), and ask it (`ask`) on a task of its
/// own, inside `bound`. Returns once every ask has answered.
///
/// An ask starts once a lane is free and `bound.spacing` has passed since
/// the start before it, or as soon as a lane is free when the scan is already
/// waiting for it. A service hears what is being asked for it before the ask
/// starts, and the answer when it comes. One nobody will read (its receiver
/// is gone: the loop needed nothing, or asked for itself) is not asked, and
/// an ask in flight for it is dropped. `patience_left` is read before each
/// start: once it says no, this service and every one after it are left
/// unasked.
///
/// Dropping this future drops every ask in flight with it.
async fn drain<Service, Planned, Answer, Plan, PlanFut, Ask, AskFut>(
    queue: Vec<Queued<Service, Answer>>,
    bound: Bound,
    patience_left: impl Fn() -> bool,
    plan: Plan,
    ask: Ask,
) -> Drained
where
    Plan: Fn(Service) -> PlanFut,
    PlanFut: Future<Output = Option<(String, Planned)>>,
    Ask: Fn(Planned) -> AskFut,
    AskFut: Future<Output = Answer> + Send + 'static,
    Answer: Send + 'static,
{
    let lanes = Arc::new(Semaphore::new(bound.in_flight.max(1)));
    let mut asking = tokio::task::JoinSet::new();
    let mut next_start = tokio::time::Instant::now();
    let mut drained = Drained::default();
    let total = queue.len();
    for (position, queued) in queue.into_iter().enumerate() {
        let Queued {
            service,
            wanted,
            mut identity,
            mut answer,
        } = queued;
        // The loop is past this service already: nothing to read for it.
        if identity.is_closed() {
            continue;
        }
        let Some((asks, planned)) = plan(service).await else {
            continue;
        };
        let lane = lanes
            .clone()
            .acquire_owned()
            .await
            .expect("the lanes are never closed");
        if !patience_left() {
            drained.left_to_the_loop = total - position;
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(next_start) => {}
            _ = wanted.notified() => {}
            _ = identity.closed() => {}
        }
        if identity.send(asks).is_err() {
            continue;
        }
        next_start = tokio::time::Instant::now() + bound.spacing;
        let asked = ask(planned);
        // Quiet about a panic: the sender drops with the task, the loop hears
        // nothing and asks for itself, where a second panic is the scan's.
        asking.spawn(crate::panic_report::quiet(async move {
            let _lane = lane;
            // Whether anyone will read the answer is looked at first, so an
            // ask nobody waits for is never begun.
            let answered = tokio::select! {
                biased;
                _ = answer.closed() => None,
                answered = asked => Some(answered),
            };
            if let Some(answered) = answered {
                let _ = answer.send(answered);
            }
        }));
        drained.asked += 1;
        while asking.try_join_next().is_some() {}
    }
    while asking.join_next().await.is_some() {}
    drained
}

/// What the queue asks for one service.
struct AskAhead {
    service: Option<String>,
    packages: Packages,
    import_facts: ImportSample,
    settled: Option<SettledDetection>,
}

/// One service as the queue starts with it.
struct ServiceAhead {
    config: Config,
    stored: Option<StoredSetup>,
}

/// Run `read` with nothing it logs written anywhere.
///
/// The queue reads a service's manifests and files before the loop does, with
/// the loop's own readers, and those warn about what they find: a workspace
/// file that does not parse, a manifest that is ignored. The loop reads the
/// same manifests and files when it reaches the service and gives the same
/// warnings there. Said by both, each would reach the terminal twice.
fn quietly<Read>(read: impl FnOnce() -> Read) -> Read {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), read)
}

/// Read what `service` would ask, as the loop reads it: its manifests, the
/// decision on its previous generation, and its import sample. `None` when it
/// asks nothing ahead: the incremental branch replays its setup, or the read
/// failed, which the loop meets and reports where it always did.
fn read_ask(repo_path: &str, service: ServiceAhead) -> Option<(String, AskAhead)> {
    let packages = super::load_packages_for_service(repo_path, &service.config).ok()?;
    let canonical = super::canonical_repo_path(repo_path);
    let package_json_hash = super::hash_workspace_package_jsons(&packages, &canonical).ok()?;
    let settled = match service.stored {
        None => None,
        Some(stored) => match stored.decide(&package_json_hash) {
            SetupAsk::Reuse { .. } => return None,
            SetupAsk::Ask(settled) => settled,
        },
    };
    // A kept detection is not asked again, so its sample is not read.
    let import_facts = match &settled {
        Some(_) => ImportSample::default(),
        None => super::read_import_sample(&canonical, &service.config)?,
    };
    let identity = ask_identity(&packages, &import_facts, settled.as_ref());
    Some((
        identity,
        AskAhead {
            service: service.config.service_name.clone(),
            packages,
            import_facts,
            settled,
        },
    ))
}

/// The queue's task, aborted when dropped.
struct QueueTask(tokio::task::JoinHandle<()>);

impl Drop for QueueTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Every service's model setup, asked for ahead of its analysis. See the
/// module.
pub(super) struct SetupsAhead {
    /// One per service, in the scan's order, taken when the loop reaches it.
    slots: Mutex<Vec<Option<Slot<SetupAnswer>>>>,
    turns: AskTurns,
    _queue: Option<QueueTask>,
}

impl SetupsAhead {
    /// Start asking for the setups of `services`, in order. `stored` is the
    /// part of a service's previous generation its setup is decided on.
    ///
    /// Asks nothing when the scan uses no model, or [`IN_FLIGHT_ENV`] is `0`.
    pub(super) fn start(
        repo_path: &str,
        services: &[Config],
        stored: impl Fn(&Config) -> Option<StoredSetup>,
    ) -> Self {
        let turns = AskTurns::default();
        let bound = match Bound::of_run() {
            Some(bound) if !crate::local_mode::no_model() => bound,
            _ => {
                return Self {
                    slots: Mutex::new(Vec::new()),
                    turns,
                    _queue: None,
                };
            }
        };
        let (queue, slots): (Vec<_>, Vec<_>) = services
            .iter()
            .map(|config| {
                let (queued, slot) = queue_place(ServiceAhead {
                    config: config.clone(),
                    stored: stored(config),
                });
                (queued, Some(slot))
            })
            .unzip();
        let repo_path = repo_path.to_string();
        let ask_turns = turns.clone();
        let started = std::time::Instant::now();
        let total = services.len();
        let task = tokio::spawn(async move {
            let drained = drain(
                queue,
                bound,
                || !crate::retry_budget::remaining().is_zero(),
                move |service: ServiceAhead| {
                    let repo_path = repo_path.clone();
                    async move {
                        // The read walks and parses the service's files, so
                        // it runs off the runtime's own threads.
                        tokio::task::spawn_blocking(move || {
                            quietly(|| read_ask(&repo_path, service))
                        })
                        .await
                        .ok()
                        .flatten()
                    }
                },
                move |ask: AskAhead| {
                    let turns = ask_turns.clone();
                    crate::current_service::asked_for(ask.service.clone(), async move {
                        super::ask_model_setup(
                            &ask.packages,
                            &ask.import_facts,
                            ask.settled,
                            &turns,
                        )
                        .await
                    })
                },
            )
            .await;
            if drained.left_to_the_loop > 0 {
                info!("{}", left_to_the_loop_line(drained.left_to_the_loop, total));
            }
            if drained.asked > 0 {
                info!(
                    "{}",
                    asked_ahead_line(drained.asked, total, bound, started.elapsed())
                );
            }
        });
        Self {
            slots: Mutex::new(slots),
            turns,
            _queue: Some(QueueTask(task)),
        }
    }

    /// Where the service at `index` gets its model setup on this pass.
    ///
    /// An answer asked ahead is for the pass that reads the stored generation.
    /// A retry of owed work reads this run's own, asks only what it lacks, and
    /// asks for itself.
    pub(super) fn source(&self, index: usize, generation: PreviousGeneration) -> SetupSource {
        let ahead = match generation {
            PreviousGeneration::Stored => self
                .slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(index)
                .and_then(Option::take),
            PreviousGeneration::ThisRun => None,
        };
        SetupSource {
            ahead,
            turns: self.turns.clone(),
        }
    }
}

/// The run log's line for a queue that asked: how many, how wide, how long.
fn asked_ahead_line(asked: usize, total: usize, bound: Bound, took: Duration) -> String {
    format!(
        "Asked for the detection and guidance of {asked} of {total} service(s) ahead of their \
         analysis, {} at a time, in {:.1}s",
        bound.in_flight,
        took.as_secs_f64()
    )
}

/// The run log's line for a queue that stopped early.
fn left_to_the_loop_line(left: usize, total: usize) -> String {
    format!(
        "This run has spent its {}s retry budget ({}), so nothing more is asked ahead: the \
         detection and guidance of the last {left} of {total} service(s) wait until the scan \
         reaches them",
        crate::retry_budget::budget().as_secs(),
        crate::retry_budget::BUDGET_ENV
    )
}

/// Where one service's model setup comes from on one pass: the answer asked
/// ahead, when it was asked what this pass would ask, otherwise an ask of its
/// own.
pub(super) struct SetupSource {
    ahead: Option<Slot<SetupAnswer>>,
    turns: AskTurns,
}

impl SetupSource {
    /// The service's model setup: detection, guidance and extraction config,
    /// or a deferral.
    ///
    /// Never fails. Detection and guidance are single calls a whole service
    /// depends on, so they retry under
    /// [`crate::agent_service::RetryPolicy::PATIENT`]; when even that is
    /// spent, the service's model analysis is DEFERRED rather than the run
    /// ended: its files are analysed facts-only, none of the missing answers
    /// is cached, and the engine names it at the end and asks again
    /// (2026-09-15: one exhausted detection call aborted a seven-service first
    /// index after four services were done). A guidance failure keeps the
    /// detection that answered, so the next ask is guidance only
    /// (carrick#1126).
    ///
    /// A deferred service is never analysed under a stand-in guidance. The
    /// analyzer's cache key names the guidance it embedded, so answers bought
    /// under a placeholder would be paid for again the moment the real
    /// guidance arrived.
    pub(super) async fn settle(
        self,
        packages: &Packages,
        import_facts: &ImportSample,
        settled: Option<SettledDetection>,
    ) -> ModelSetup {
        // The stage is the scan's, set where the scan waits: an ask made
        // ahead runs beside whatever stage the loop is in.
        crate::scan_stage::enter(crate::scan_stage::Stage::FrameworkDetect);
        let wanted = ask_identity(packages, import_facts, settled.as_ref());
        let answer = match answer_asked_ahead(self.ahead, &wanted).await {
            Some(answer) => answer,
            None => super::ask_model_setup(packages, import_facts, settled, &self.turns).await,
        };
        answer.into_setup()
    }
}

/// The answer asked ahead for a service, when one was and it was asked
/// `wanted`. `None` leaves the service to ask for itself: the queue asked
/// nothing for it, asked something else (the tree moved under the scan), or
/// ended before it answered.
async fn answer_asked_ahead<Answer>(ahead: Option<Slot<Answer>>, wanted: &str) -> Option<Answer> {
    let Slot {
        wanted: waiting_for_it,
        identity,
        answer,
    } = ahead?;
    let waiting = std::time::Instant::now();
    // Kept until the queue reads it, so it is heard whichever comes first.
    waiting_for_it.notify_one();
    let asked = identity.await.ok()?;
    if asked != wanted {
        // Dropping the slot here drops the ask with it.
        debug!(
            "What was asked ahead for this service is not what it asks now; asking again for \
             itself"
        );
        return None;
    }
    let answer = answer.await.ok()?;
    debug!(
        "This service's detection and guidance were asked for ahead of it; waited {:.1}s for the \
         answer",
        waiting.elapsed().as_secs_f64()
    );
    Some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::time::{Instant, sleep};

    /// The run's bound, with the width a test asks for.
    fn bound(in_flight: usize) -> Bound {
        Bound {
            in_flight,
            spacing: START_SPACING,
        }
    }

    /// A queue of `count` services, each named by its position, and the
    /// places the loop holds in it.
    fn places<Answer>(count: usize) -> (Vec<Queued<usize, Answer>>, Vec<Slot<Answer>>) {
        (0..count).map(queue_place).unzip()
    }

    /// A read that asks for every service, under its position as its id.
    async fn asks_as_it_is(service: usize) -> Option<(String, usize)> {
        Some((service.to_string(), service))
    }

    fn seconds(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn millis(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The knob lowers the width and never raises it: the maximum is what
    /// keeps a route's requests inside what the cloud takes at once.
    #[test]
    fn the_knob_sets_the_width_up_to_the_maximum_and_nought_asks_none_ahead() {
        let width = |value: Option<&str>| Bound::read(value).map(|bound| bound.in_flight);
        assert_eq!(width(None), Some(MAX_IN_FLIGHT));
        assert_eq!(width(Some("2")), Some(2));
        assert_eq!(width(Some(" 1 ")), Some(1));
        assert_eq!(width(Some("400")), Some(MAX_IN_FLIGHT));
        assert_eq!(width(Some("many")), Some(MAX_IN_FLIGHT));
        assert_eq!(width(Some("0")), None);
        assert_eq!(
            Bound::read(None).map(|bound| bound.spacing),
            Some(START_SPACING)
        );
    }

    /// Ten setups of ten seconds each, with nobody waiting for any of them:
    /// four are asked at once and no more, each starts three quarters of a
    /// second after the one before, and the fifth takes the lane the first
    /// leaves.
    #[tokio::test(start_paused = true)]
    async fn no_more_setups_are_asked_at_once_than_the_bound_and_they_start_spaced() {
        let (queue, _slots) = places::<()>(10);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let starts = Arc::new(Mutex::new(Vec::new()));
        let began = Instant::now();

        let drained = drain(queue, bound(4), || true, asks_as_it_is, {
            let (in_flight, most, starts) = (in_flight.clone(), most.clone(), starts.clone());
            move |_service: usize| {
                let (in_flight, most, starts) = (in_flight.clone(), most.clone(), starts.clone());
                async move {
                    starts.lock().unwrap().push(began.elapsed());
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    sleep(seconds(10)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })
        .await;

        assert_eq!(
            drained,
            Drained {
                asked: 10,
                left_to_the_loop: 0
            }
        );
        assert_eq!(most.load(Ordering::SeqCst), 4);
        assert_eq!(
            *starts.lock().unwrap(),
            [
                0, 750, 1_500, 2_250, 10_000, 10_750, 11_500, 12_250, 20_000, 20_750
            ]
            .map(millis)
        );
    }

    /// The case the ticket was written from: one service's detection is
    /// refused for six minutes. The services behind it are answered in
    /// seconds, where they used to wait their turn behind it.
    #[tokio::test(start_paused = true)]
    async fn a_setup_the_model_keeps_refusing_holds_up_no_service_behind_it() {
        const REFUSED: usize = 1;
        let (queue, slots) = places::<usize>(6);
        let began = Instant::now();
        let _queue = tokio::spawn(drain(
            queue,
            bound(4),
            || true,
            asks_as_it_is,
            |service: usize| async move {
                sleep(seconds(if service == REFUSED { 360 } else { 5 })).await;
                service
            },
        ));

        // When each answer is there, whatever order the loop reads them in.
        // Each place is held whole until then, as the loop holds it: a place
        // let go of is a service nobody will read an answer for.
        let answered: Vec<_> = slots
            .into_iter()
            .map(|slot| {
                tokio::spawn(async move {
                    let Slot {
                        wanted: _not_yet_waited_for,
                        identity: _what_is_asked,
                        answer,
                    } = slot;
                    let service = answer.await.expect("asked ahead");
                    (service, began.elapsed())
                })
            })
            .collect();
        let mut answered_at = Vec::new();
        for answer in answered {
            answered_at.push(answer.await.unwrap());
        }

        assert_eq!(
            answered_at,
            [
                (0, millis(5_000)),
                (REFUSED, millis(360_750)),
                (2, millis(6_500)),
                (3, millis(7_250)),
                // The lanes the first and the third left.
                (4, millis(10_000)),
                (5, millis(11_500)),
            ]
        );
    }

    /// A setup the scan is waiting for is not held by the spacing: it starts
    /// when the scan asks for it, which is when it was asked before there was
    /// a queue. Three services whose setups answer at once are all asked at
    /// the moment the scan began, not three quarters of a second apart.
    #[tokio::test(start_paused = true)]
    async fn the_setup_the_scan_is_waiting_for_starts_at_once() {
        let (queue, slots) = places::<Duration>(3);
        let began = Instant::now();
        let _queue = tokio::spawn(drain(
            queue,
            bound(4),
            || true,
            asks_as_it_is,
            move |_service: usize| async move { began.elapsed() },
        ));

        let mut started = Vec::new();
        for (service, slot) in slots.into_iter().enumerate() {
            started.push(
                answer_asked_ahead(Some(slot), &service.to_string())
                    .await
                    .expect("asked ahead"),
            );
        }

        assert_eq!(started, [Duration::ZERO; 3]);
    }

    /// The loop drops its place for a service it asks nothing for (its
    /// previous generation holds its setup). The queue does not ask for it.
    #[tokio::test(start_paused = true)]
    async fn a_service_nobody_will_read_an_answer_for_is_not_asked() {
        let (queue, mut slots) = places::<usize>(3);
        drop(slots.remove(1));
        let asked = Arc::new(Mutex::new(Vec::new()));

        let drained = drain(queue, bound(4), || true, asks_as_it_is, {
            let asked = asked.clone();
            move |service: usize| {
                let asked = asked.clone();
                async move {
                    asked.lock().unwrap().push(service);
                    service
                }
            }
        })
        .await;

        assert_eq!(drained.asked, 2);
        assert_eq!(*asked.lock().unwrap(), [0, 2]);
    }

    /// What was asked ahead is not what the service asks when the scan
    /// reaches it (a file changed under the scan). The answer is not taken,
    /// the service asks for itself, and the ask made ahead is dropped where
    /// it stands rather than run to its end for nobody.
    #[tokio::test(start_paused = true)]
    async fn an_answer_to_something_else_is_not_taken_and_its_ask_is_dropped() {
        let (queue, mut slots) = places::<usize>(1);
        let finished = Arc::new(AtomicUsize::new(0));
        let began = Instant::now();
        let queue = tokio::spawn(drain(queue, bound(4), || true, asks_as_it_is, {
            let finished = finished.clone();
            move |service: usize| {
                let finished = finished.clone();
                async move {
                    sleep(seconds(60)).await;
                    finished.fetch_add(1, Ordering::SeqCst);
                    service
                }
            }
        }));

        sleep(seconds(1)).await;
        let taken = answer_asked_ahead(slots.pop(), "what the service asks now").await;
        let drained = queue.await.unwrap();

        assert_eq!(taken, None);
        assert_eq!(drained.asked, 1);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        assert!(
            began.elapsed() < seconds(2),
            "the ask was dropped when its answer was refused, not a minute later"
        );
    }

    /// Detection and guidance wait out a refusing model on the run's retry
    /// budget. Once it is spent a setup gets one attempt and no wait, so the
    /// queue asks no further: each service left asks when the loop reaches
    /// it, minutes later, as it did before there was a queue.
    #[tokio::test(start_paused = true)]
    async fn the_queue_asks_no_further_once_the_runs_patience_is_spent() {
        let (queue, slots) = places::<usize>(5);
        let patience = Arc::new(AtomicBool::new(true));

        let drained = drain(
            queue,
            bound(4),
            {
                let patience = patience.clone();
                move || patience.load(Ordering::SeqCst)
            },
            asks_as_it_is,
            move |service: usize| {
                // The second setup's wait is the one that spends the budget.
                if service == 1 {
                    patience.store(false, Ordering::SeqCst);
                }
                async move {
                    sleep(seconds(5)).await;
                    service
                }
            },
        )
        .await;

        assert_eq!(
            drained,
            Drained {
                asked: 2,
                left_to_the_loop: 3
            }
        );
        let mut heard = Vec::new();
        for (service, slot) in slots.into_iter().enumerate() {
            heard.push(answer_asked_ahead(Some(slot), &service.to_string()).await);
        }
        assert_eq!(heard, [Some(0), Some(1), None, None, None]);
    }

    /// A scan that stops (an error, an interrupt) drops its queue. Every ask
    /// in flight is dropped with it and no other is started, so a scan that
    /// ended asks nothing more.
    #[tokio::test(start_paused = true)]
    async fn a_scan_that_stops_drops_every_ask_in_flight_and_starts_no_other() {
        let (queue, _slots) = places::<()>(8);
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let task = QueueTask(tokio::spawn({
            let (started, finished) = (started.clone(), finished.clone());
            async move {
                drain(
                    queue,
                    bound(4),
                    || true,
                    asks_as_it_is,
                    move |_service: usize| {
                        let (started, finished) = (started.clone(), finished.clone());
                        async move {
                            started.fetch_add(1, Ordering::SeqCst);
                            sleep(seconds(60)).await;
                            finished.fetch_add(1, Ordering::SeqCst);
                        }
                    },
                )
                .await;
            }
        }));

        // Three have started: at 0, 0.75 and 1.5 seconds.
        sleep(seconds(2)).await;
        drop(task);
        sleep(seconds(600)).await;

        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }

    /// An ask made ahead is an early start, never the only chance: one that
    /// panics tells its service nothing, the service asks for itself, and the
    /// queue goes on to the next.
    #[tokio::test(start_paused = true)]
    async fn an_ask_that_panics_leaves_its_service_to_ask_for_itself() {
        let (queue, slots) = places::<usize>(2);
        let _queue = tokio::spawn(drain(
            queue,
            bound(4),
            || true,
            asks_as_it_is,
            |service: usize| async move {
                assert!(service != 0, "the first ask fails on purpose");
                service
            },
        ));

        let mut heard = Vec::new();
        for (service, slot) in slots.into_iter().enumerate() {
            heard.push(answer_asked_ahead(Some(slot), &service.to_string()).await);
        }

        assert_eq!(heard, [None, Some(1)]);
    }

    /// The cloud serves the second ask of a thing from what the first left,
    /// and has nothing kept until the first has answered. Three asks of one
    /// thing made together go one after another, in the order they were made,
    /// and an ask of another thing goes beside them.
    #[tokio::test(start_paused = true)]
    async fn asks_of_one_thing_take_turns_and_asks_of_other_things_go_beside_them() {
        let turns = AskTurns::default();
        let began = Instant::now();
        let ask = |what: &'static str, takes: u64| {
            let turns = turns.clone();
            async move {
                let _turn = turns.take(what.to_string()).await;
                let started = began.elapsed();
                sleep(seconds(takes)).await;
                (started, began.elapsed())
            }
        };

        let (first, second, third, other) = tokio::join!(
            ask("guidance for one set of lists", 10),
            ask("guidance for one set of lists", 1),
            ask("guidance for one set of lists", 1),
            ask("guidance for another", 3),
        );

        assert_eq!(first, (seconds(0), seconds(10)));
        assert_eq!(second, (seconds(10), seconds(11)));
        assert_eq!(third, (seconds(11), seconds(12)));
        assert_eq!(other, (seconds(0), seconds(3)));
    }

    fn detection(frameworks: &[&str], data_fetchers: &[&str]) -> DetectionResult {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect();
        DetectionResult {
            frameworks: names(frameworks),
            data_fetchers: names(data_fetchers),
            ..DetectionResult::default()
        }
    }

    /// The cloud keys a guidance answer on its lists as sets, so two services
    /// that detected the same packages in another order ask the same thing
    /// and take turns, and a service that detected one more does not wait for
    /// them.
    #[test]
    fn two_services_ask_the_same_guidance_when_their_lists_hold_the_same_names() {
        let one = detection(&["express", "koa"], &["axios"]);
        let reordered = detection(&["koa", "express", "koa"], &["axios"]);
        let another = detection(&["express", "koa"], &["axios", "got"]);
        assert_eq!(guidance_ask(&one), guidance_ask(&reordered));
        assert_ne!(guidance_ask(&one), guidance_ask(&another));
        // A name moved from one list to the other is another ask.
        assert_ne!(
            guidance_ask(&detection(&["axios"], &[])),
            guidance_ask(&detection(&[], &["axios"]))
        );
    }

    /// A guidance that can be replayed: one with an id.
    fn keyed_guidance() -> ProtocolGuidance {
        let mut guidance = crate::local_mode::offline_guidance();
        for answer in guidance.values_mut() {
            answer.guidance_key = Some("an id".to_string());
        }
        guidance
    }

    const MANIFESTS: &str = "the hash of these manifests";

    /// A previous generation of this cache version, from [`MANIFESTS`], that
    /// holds a detection and a guidance that can be replayed.
    fn complete() -> StoredSetup {
        StoredSetup {
            cache_version: Some(CACHE_VERSION),
            package_json_hash: Some(MANIFESTS.to_string()),
            detection: Some(detection(&["express"], &["axios"])),
            guidance: Some(keyed_guidance()),
            extraction_config: Some(ExtractionConfig::default()),
        }
    }

    /// What `stored` is asked under [`MANIFESTS`]: `None` when its setup is
    /// replayed, otherwise whether a kept detection goes with the ask.
    fn asked(stored: StoredSetup) -> Option<Option<DetectionResult>> {
        match stored.decide(MANIFESTS) {
            SetupAsk::Reuse { .. } => None,
            SetupAsk::Ask(settled) => Some(settled.map(|settled| settled.detection)),
        }
    }

    /// The decision the incremental branch and the queue both read. A setup
    /// is replayed only from a generation that holds all of it, written by
    /// this cache version from these manifests; everything else asks, and
    /// keeps a detection only when the manifests and the version are the ones
    /// it was asked under.
    #[test]
    fn a_setup_is_replayed_only_from_a_generation_that_holds_all_of_it() {
        assert!(asked(complete()).is_none(), "a complete generation replays");
        // The manifests moved: everything is asked again.
        assert!(matches!(
            StoredSetup {
                package_json_hash: Some("other manifests".to_string()),
                ..complete()
            }
            .decide(MANIFESTS),
            SetupAsk::Ask(None)
        ));
        // Written by another cache version: a full analysis, which asks all.
        assert!(matches!(
            StoredSetup {
                cache_version: Some(CACHE_VERSION - 1),
                ..complete()
            }
            .decide(MANIFESTS),
            SetupAsk::Ask(None)
        ));
        // Its guidance was deferred: the detection is kept, guidance asked.
        assert_eq!(
            asked(StoredSetup {
                guidance: None,
                ..complete()
            })
            .map(|kept| kept.map(|detection| detection.frameworks)),
            Some(Some(vec!["express".to_string()]))
        );
        // Its guidance has no id (written before the id existed): the same.
        assert!(matches!(
            StoredSetup {
                guidance: Some(crate::local_mode::offline_guidance()),
                ..complete()
            }
            .decide(MANIFESTS),
            SetupAsk::Ask(Some(_))
        ));
        // Its detection was deferred: all of it is asked.
        assert!(matches!(
            StoredSetup {
                detection: None,
                guidance: None,
                ..complete()
            }
            .decide(MANIFESTS),
            SetupAsk::Ask(None)
        ));
    }

    fn write(root: &std::path::Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A service that loads modules every way the import sample reads: a
    /// binding, a side-effect import, a re-export, a `require`, an `import()`,
    /// and one file that does not parse.
    fn a_service() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            "package.json",
            r#"{ "name": "a-service", "dependencies": { "express": "4.18.2", "axios": "1.7.0" } }"#,
        );
        write(
            repo.path(),
            "src/server.ts",
            "import express from \"express\";\nimport \"reflect-metadata\";\n\
             export * from \"./client\";\nconst queue = require(\"bullmq\");\n\
             export const app = express();\nexport const load = () => import(\"pg\");\n",
        );
        write(
            repo.path(),
            "src/client.ts",
            "import axios from \"axios\";\nexport const status = () => axios.get(\"/status\");\n",
        );
        write(repo.path(), "src/broken.ts", "export const = ;\n");
        repo
    }

    fn ahead(stored: Option<StoredSetup>) -> ServiceAhead {
        ServiceAhead {
            config: Config::default(),
            stored,
        }
    }

    /// What the loop reads for the service at `repo`: its manifests, their
    /// hash, and discovery's import sample.
    fn as_the_scan_reads_it(repo: &std::path::Path) -> (Packages, String, ImportSample) {
        let repo_path = repo.to_string_lossy();
        let service = Config::default();
        let packages = super::super::load_packages_for_service(&repo_path, &service).unwrap();
        let canonical = super::super::canonical_repo_path(&repo_path);
        let hash = super::super::hash_workspace_package_jsons(&packages, &canonical).unwrap();
        let discovered =
            super::super::discover_files_and_symbols(&canonical, &service, Default::default())
                .unwrap();
        (packages, hash, discovered.import_facts)
    }

    /// The loop takes an answer asked ahead only when it was asked what the
    /// loop would send. This is that condition on a real service: what the
    /// queue reads for it, with its own walk and its own parse, is what
    /// discovery reads when the scan reaches it.
    #[test]
    fn what_is_read_ahead_for_a_service_is_what_the_scan_reads_for_it() {
        let repo = a_service();
        let (packages, _, import_facts) = as_the_scan_reads_it(repo.path());

        let (identity, ask) =
            read_ask(&repo.path().to_string_lossy(), ahead(None)).expect("a first index asks");

        assert!(ask.import_facts.fact_count() > 0);
        assert_eq!(ask.import_facts, import_facts);
        assert_eq!(identity, ask_identity(&packages, &import_facts, None));
        assert!(ask.settled.is_none());
    }

    /// A rescan sends what it sent before: a service whose previous
    /// generation holds its setup from these manifests is asked nothing
    /// ahead, and one whose manifests have moved is asked all of it.
    #[test]
    fn a_service_that_holds_its_setup_is_asked_nothing_ahead() {
        let repo = a_service();
        let repo_path = repo.path().to_string_lossy().to_string();
        let (_, hash, _) = as_the_scan_reads_it(repo.path());
        let held = StoredSetup {
            package_json_hash: Some(hash),
            ..complete()
        };

        assert!(read_ask(&repo_path, ahead(Some(held.clone()))).is_none());

        write(
            repo.path(),
            "package.json",
            r#"{ "name": "a-service", "dependencies": { "express": "4.18.2", "got": "14.0.0" } }"#,
        );
        let (_, ask) = read_ask(&repo_path, ahead(Some(held))).expect("the manifests moved");
        assert!(
            ask.settled.is_none(),
            "a detection from other manifests is not kept"
        );
    }

    /// A service whose guidance was deferred asks for the guidance alone, and
    /// what it asks does not depend on its files: its detection is not asked
    /// again, so its sample is not read.
    #[test]
    fn a_service_that_kept_its_detection_asks_ahead_for_what_follows_from_it() {
        let repo = a_service();
        let repo_path = repo.path().to_string_lossy().to_string();
        let (packages, hash, import_facts) = as_the_scan_reads_it(repo.path());
        let deferred = StoredSetup {
            package_json_hash: Some(hash.clone()),
            guidance: None,
            ..complete()
        };

        let (identity, ask) =
            read_ask(&repo_path, ahead(Some(deferred.clone()))).expect("its guidance is owed");

        let kept = deferred.kept(&hash);
        assert!(kept.is_some());
        assert_eq!(ask.import_facts, ImportSample::default());
        assert_eq!(
            identity,
            ask_identity(&packages, &import_facts, kept.as_ref())
        );
        assert_ne!(identity, ask_identity(&packages, &import_facts, None));
    }

    /// Counts every event it is given.
    struct Count(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
        fn on_event(
            &self,
            _event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The readers the queue runs ahead of the loop warn about what they
    /// find, and the loop runs the same readers over the same service. A
    /// warning is given once, by the loop: what the read made ahead would log
    /// reaches nothing.
    #[test]
    fn what_the_read_made_ahead_would_log_is_written_nowhere() {
        use tracing_subscriber::layer::SubscriberExt;
        let events = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(events.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("said by the loop's own read");
            assert_eq!(events.load(Ordering::SeqCst), 1);

            let read = quietly(|| {
                tracing::warn!("said by the read made ahead");
                "what was read"
            });

            assert_eq!(read, "what was read");
            assert_eq!(events.load(Ordering::SeqCst), 1);
            tracing::warn!("said by the loop again, once the read is over");
            assert_eq!(events.load(Ordering::SeqCst), 2);
        });
    }

    /// Discovery refuses a service with no source file, and the loop reports
    /// it there. Nothing is asked for it first.
    #[test]
    fn a_service_with_no_source_file_is_asked_nothing_ahead() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "package.json", r#"{ "name": "empty" }"#);
        assert!(read_ask(&repo.path().to_string_lossy(), ahead(None)).is_none());
    }

    /// One service's timings, in tenths of a second: its read before the
    /// model is asked, its detection, its guidance, its extraction config,
    /// which set of detected lists it asked guidance for (services that share
    /// a number asked the same guidance), and the rest of its analysis.
    type Logged = (u64, u64, u64, u64, usize, u64);

    /// The services of the first index this queue was written from, in the
    /// order they were analysed, as the scanner's own log timed them.
    ///
    /// The forty-second is the detection the model refused for capacity.
    /// Guidance in a fraction of a second is the cloud answering from what an
    /// earlier service's ask left; set 13 was asked twice in full, because
    /// the first answer was not kept.
    const LOGGED: [Logged; 43] = [
        (1, 58, 178, 53, 0, 191),
        (1, 40, 147, 39, 1, 7),
        (1, 145, 142, 22, 2, 139),
        (2, 99, 248, 44, 3, 1104),
        (2, 32, 7, 35, 0, 17),
        (1, 16, 167, 16, 4, 222),
        (2, 15, 3, 19, 1, 63),
        (2, 13, 2, 15, 1, 16),
        (1, 16, 2, 30, 1, 57),
        (1, 18, 139, 237, 5, 68),
        (2, 56, 133, 82, 6, 170),
        (3, 52, 143, 27, 7, 143),
        (2, 59, 126, 64, 8, 165),
        (2, 2, 3, 2, 1, 147),
        (2, 17, 538, 27, 9, 18),
        (2, 23, 289, 29, 10, 1046),
        (2, 17, 4, 20, 1, 53),
        (1, 18, 1, 21, 1, 211),
        (2, 21, 2, 16, 1, 336),
        (2, 35, 133, 23, 11, 319),
        (2, 73, 168, 23, 12, 1242),
        (2, 20, 5, 328, 1, 125),
        (2, 54, 136, 23, 13, 59),
        (2, 18, 463, 23, 14, 1911),
        (1, 28, 268, 27, 15, 136),
        (1, 19, 545, 21, 16, 1738),
        (1, 37, 589, 24, 17, 454),
        (2, 38, 460, 229, 13, 5),
        (1, 22, 2, 335, 1, 135),
        (2, 18, 3, 18, 1, 163),
        (2, 70, 285, 121, 18, 595),
        (2, 21, 158, 30, 19, 153),
        (2, 83, 187, 37, 20, 144),
        (2, 17, 2, 14, 1, 195),
        (12, 111, 274, 33, 21, 13852),
        (7, 163, 259, 69, 22, 3023),
        (2, 44, 12, 24, 1, 363),
        (1, 18, 149, 22, 23, 12),
        (1, 19, 4, 26, 0, 986),
        (2, 19, 302, 32, 24, 225),
        (4, 20, 159, 277, 25, 3346),
        (1, 3580, 187, 64, 26, 182),
        (1, 56, 150, 116, 27, 3199),
    ];

    fn tenths(n: u64) -> Duration {
        millis(n * 100)
    }

    /// What a replay of [`LOGGED`] came to.
    struct Replayed {
        /// How long the loop waited for each service's setup.
        waited: Vec<Duration>,
        /// When the queue had every answer.
        queue_took: Duration,
        /// When the last service's analysis ended.
        pass_took: Duration,
        drained: Drained,
    }

    /// Replay [`LOGGED`] through the queue and a loop that analyses the
    /// services in order, every request taking what it took.
    ///
    /// The n-th ask of one set of lists takes what the n-th ask of it took in
    /// the log: the first is answered by the model and the rest from what the
    /// cloud kept, whichever service comes to ask first.
    async fn replay(bound: Bound) -> Replayed {
        let turns = AskTurns::default();
        let mut logged_guidance: HashMap<usize, VecDeque<u64>> = HashMap::new();
        for (_, _, guidance, _, lists, _) in LOGGED {
            logged_guidance
                .entry(lists)
                .or_default()
                .push_back(guidance);
        }
        let logged_guidance = Arc::new(Mutex::new(logged_guidance));
        let (queue, slots) = places::<()>(LOGGED.len());
        let began = Instant::now();

        let queue = tokio::spawn(async move {
            let drained = drain(
                queue,
                bound,
                || true,
                |service: usize| async move {
                    sleep(tenths(LOGGED[service].0)).await;
                    Some((service.to_string(), service))
                },
                move |service: usize| {
                    let (turns, logged_guidance) = (turns.clone(), logged_guidance.clone());
                    async move {
                        let (_, detection, _, extraction, lists, _) = LOGGED[service];
                        sleep(tenths(detection)).await;
                        tokio::join!(
                            async {
                                let _turn = turns.take(format!("guidance {lists}")).await;
                                let took = logged_guidance
                                    .lock()
                                    .unwrap()
                                    .get_mut(&lists)
                                    .and_then(VecDeque::pop_front)
                                    .expect("one logged time for each ask");
                                sleep(tenths(took)).await;
                            },
                            sleep(tenths(extraction)),
                        );
                    }
                },
            )
            .await;
            (drained, began.elapsed())
        });

        let mut waited = Vec::new();
        for (service, slot) in slots.into_iter().enumerate() {
            let (read, .., rest) = LOGGED[service];
            sleep(tenths(read)).await;
            let waiting = Instant::now();
            answer_asked_ahead(Some(slot), &service.to_string())
                .await
                .expect("asked ahead");
            waited.push(waiting.elapsed());
            sleep(tenths(rest)).await;
        }
        let pass_took = began.elapsed();
        let (drained, queue_took) = queue.await.unwrap();
        Replayed {
            waited,
            queue_took,
            pass_took,
            drained,
        }
    }

    /// What each logged service waited for its setup when the loop asked for
    /// it: its detection, then its guidance and extraction config together.
    fn waited_in_the_log() -> Vec<Duration> {
        LOGGED
            .iter()
            .map(|(_, detection, guidance, extraction, _, _)| {
                tenths(detection + guidance.max(extraction))
            })
            .collect()
    }

    /// The first index of 43 services the queue was written from, replayed
    /// with every request taking what it took.
    ///
    /// As it ran, the loop waited 22.6 minutes for setups, one service after
    /// another, 6 of them for the one detection the model was refusing. Asked
    /// ahead, the loop waits for the first service's setup and no other: the
    /// refused detection is asked for from the scan's first minutes and is
    /// answered long before the loop reaches its service. The pass is shorter
    /// by what it no longer waits.
    #[tokio::test(start_paused = true)]
    async fn the_setups_of_a_large_first_index_are_asked_while_the_scan_analyses() {
        let logged = waited_in_the_log();
        let logged_wait: Duration = logged.iter().sum();
        assert_eq!(logged_wait, tenths(13_542), "22.6 minutes, as logged");
        assert_eq!(logged[41], tenths(3_767), "the refused detection's service");

        let replayed = replay(bound(MAX_IN_FLIGHT)).await;

        assert_eq!(
            replayed.drained,
            Drained {
                asked: 43,
                left_to_the_loop: 0
            }
        );
        // The first service waits for its own setup, as it did. Nothing was
        // asked before it.
        assert_eq!(replayed.waited[0], logged[0]);
        assert_eq!(
            replayed.waited[1..],
            vec![Duration::ZERO; 42],
            "every other setup was there when the loop reached its service"
        );
        let waited: Duration = replayed.waited.iter().sum();
        let logged_pass: Duration = LOGGED
            .iter()
            .map(|(read, .., rest)| tenths(read + rest))
            .sum::<Duration>()
            + logged_wait;
        assert_eq!(logged_pass - replayed.pass_took, logged_wait - waited);
        assert!(
            replayed.queue_took < seconds(12 * 60),
            "every setup was answered within twelve minutes of a pass of an hour: {:?}",
            replayed.queue_took
        );
        eprintln!(
            "replay: the loop waited {:.1}s for setups (logged {:.1}s); the queue had every \
             answer after {:.1}s; the pass took {:.1}s (logged {:.1}s)",
            waited.as_secs_f64(),
            logged_wait.as_secs_f64(),
            replayed.queue_took.as_secs_f64(),
            replayed.pass_took.as_secs_f64(),
            logged_pass.as_secs_f64()
        );
    }

    /// The same replay one setup at a time: the width is not what removes the
    /// wait, asking ahead is. Four at a time is what keeps the loop from
    /// waiting at all after its first service.
    #[tokio::test(start_paused = true)]
    async fn asked_one_at_a_time_the_setups_still_run_ahead_of_the_scan() {
        let logged_wait: Duration = waited_in_the_log().iter().sum();

        let replayed = replay(bound(1)).await;

        let waited: Duration = replayed.waited.iter().sum();
        assert_eq!(replayed.drained.asked, 43);
        assert!(
            waited < seconds(120),
            "the loop waited {waited:?} of the {logged_wait:?} it waited as logged"
        );
        assert!(
            waited > replayed.waited[0],
            "and more than at four at a time"
        );
    }
}
