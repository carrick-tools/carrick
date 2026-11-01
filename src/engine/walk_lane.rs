//! Every service's compiler walk (carrick#2413 slice G1), admitted beside
//! the analyses on one memory budget (carrick#3085).
//!
//! With `CARRICK_POSITION_ROWS=1` each service's per-position rows are
//! walked in a record-only sidecar of their own (carrick#2405). The walks
//! are queued in declared order as the pass starts, and admitted with the
//! analyses against the run's ledger
//! ([`service_workers::walks_and_analyses`]), beside the queue that asks
//! each service's setup ahead ([`super::setup_ahead`]):
//!
//! * **Each setup waits for its own walk, and no other.** A service's plan is
//!   read once its walk has ended ([`WalkEnded`]), so the first service's
//!   setup is answered while the other walks run. Asking a setup from inside
//!   the loop instead, after that service's walk, undoes carrick#1895: the
//!   loop would wait for every setup again.
//! * **Each analysis waits for the walks it reads, and no other.** Its own,
//!   and those of every service its rows reach, closed over theirs; every
//!   analysed service's when its rows may call through an unresolved import
//!   ([`ReachesOf`]). A carried service those rows reach is walked once, on
//!   demand, or filled with the rows an earlier pass kept ([`reuse`],
//!   carrick#3027).
//! * **A walk is sized by a walk's memory** ([`ServiceMemory::for_walk`]),
//!   and one that dies of memory below the run's cap is walked once more,
//!   alone, at the run's cap; its setup and its analysis wait for that walk.
//! * **The rows are kept until the run ends**, each in its service's slot
//!   ([`Slots`]): read by the service's analysis, by the services that reach
//!   it, and again by the owed-work retry, from one walk.
//!
//! Off, there is no lane: the queue and the loop run as they did.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tokio::sync::oneshot;
use tracing::{debug, info};

use super::service_workers::{self, Reached, ServiceMemory, WalkEnd};
use crate::config::Config;
use crate::position_rows::{Destination, PositionIndex};
use crate::services::type_sidecar::{MemoryDeathNote, SidecarRecipe};

/// Heard by a service's setup once its walk has ended, however it ended:
/// a walk that failed or panicked drops its sender, which is heard the same.
pub(super) type WalkEnded = oneshot::Receiver<()>;

/// Where a service's rows go this run, from the repo name and the service
/// name: [`crate::position_rows::destination`] in a scan.
pub(super) type Place = Arc<dyn Fn(&str, Option<&str>) -> Option<Destination> + Send + Sync>;

/// The services the walk of a declared service reaches, by declared
/// position ([`super::siblings::SiblingRegistry::walks_read_by`]); none when
/// no sibling is ever read.
pub(super) type ReachesOf = Arc<dyn Fn(usize, &Walked) -> BTreeSet<usize> + Send + Sync>;

/// One service's walk, as the run keeps it until it ends.
#[derive(Debug)]
pub(super) struct Walked {
    /// The rows file, when the walk wrote one. A temporary one is removed
    /// when this is dropped, which is when the run ends.
    rows: Option<Destination>,
    scan_root: PathBuf,
    label: String,
    /// The walk's sidecar at its peak, in MB (carrick#3083). `None` when
    /// nothing was walked, the walk failed, kept rows were reused, or the
    /// platform could not say.
    peak_mb: Option<u64>,
}

impl Walked {
    pub(super) fn peak_mb(&self) -> Option<u64> {
        self.peak_mb
    }

    /// The service's rows for one pass of its analysis, read from the file
    /// its walk wrote. A walk that wrote none gives an index with no rows,
    /// every lookup in which is a hole.
    pub(super) fn index(&self) -> PositionIndex {
        match &self.rows {
            Some(rows) => crate::position_rows::load_index(rows, &self.scan_root, &self.label),
            None => PositionIndex::missing(&self.scan_root),
        }
    }
}

/// Each declared service's walk, in a slot filled once when it ends and
/// read from then until the run ends (carrick#3085).
#[derive(Debug)]
pub(super) struct Slots {
    walks: Vec<OnceLock<Walked>>,
    /// A carried service whose walk is queued or running.
    walking: Vec<AtomicBool>,
}

impl Slots {
    pub(super) fn new(services: usize) -> Self {
        Slots {
            walks: (0..services).map(|_| OnceLock::new()).collect(),
            walking: (0..services).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    /// The walk of the declared service `service`, once it has ended.
    pub(super) fn get(&self, service: usize) -> Option<&Walked> {
        self.walks.get(service).and_then(OnceLock::get)
    }

    /// Whether the carried `service` is being walked and has not ended.
    pub(super) fn is_walking(&self, service: usize) -> bool {
        self.walking
            .get(service)
            .is_some_and(|walking| walking.load(Ordering::SeqCst))
            && self.get(service).is_none()
    }

    fn fill(&self, service: usize, walked: Walked) {
        if let Some(slot) = self.walks.get(service) {
            let _ = slot.set(walked);
        }
    }
}

/// Whether this run walks the rows at all.
pub(super) fn requested() -> bool {
    std::env::var(crate::position_rows::POSITION_ROWS_ENV).as_deref() == Ok("1")
}

/// One heard [`WalkEnded`] per service, and the senders the lane drops as
/// each walk ends.
pub(super) fn signals(count: usize) -> (Vec<oneshot::Sender<()>>, Vec<WalkEnded>) {
    (0..count).map(|_| oneshot::channel()).unzip()
}

/// A heap cap as the run log states it.
fn cap_text(heap_cap_mb: Option<u64>) -> String {
    heap_cap_mb.map_or_else(|| "V8's default".to_string(), |mb| format!("{mb} MB"))
}

/// One walk as it ended, on its own thread.
pub(super) struct WalkDone {
    walked: Walked,
    /// Its sidecar died of memory.
    died: bool,
    /// The services its rows reach; none when it died.
    reaches: BTreeSet<usize>,
    secs: f64,
}

/// The run's walks: how each declared service is walked and where its rows
/// land, for [`service_workers::walks_and_analyses`] (carrick#3085).
pub(super) struct Lane {
    recipe: Option<SidecarRecipe>,
    repo_path: String,
    /// Every declared service, analysed or carried.
    services: Vec<Config>,
    /// Per declared service, the commit whose inputs its rows are a walk of,
    /// recorded beside them (a clean tree's, for an analysed service), or
    /// `None` when no commit vouches for them; a carried service's is the
    /// commit it is carried at, which kept rows must be a walk of to be used
    /// instead of walked (carrick#3027).
    at: Vec<Option<String>>,
    /// Per declared service, its walk's heap cap and reservation.
    memories: Vec<ServiceMemory>,
    run_cap_mb: Option<u64>,
    /// Per declared service, the sender its setup hears drop when its walk
    /// has ended for good.
    ended: Vec<Option<oneshot::Sender<()>>>,
    slots: Arc<Slots>,
    reaches_of: ReachesOf,
    place: Place,
    /// Per carried service, the service whose rows first reached it.
    reached_by: Vec<Option<usize>>,
}

impl Lane {
    /// The walks of `services` (every declared one), each started with the
    /// heap cap of its entry in `memories` and filling its slot in `slots`.
    /// `ended` holds, by declared position, the sender each setup waits on.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        recipe: Option<SidecarRecipe>,
        repo_path: &str,
        services: &[Config],
        at: Vec<Option<String>>,
        memories: Vec<ServiceMemory>,
        run_cap_mb: Option<u64>,
        ended: Vec<Option<oneshot::Sender<()>>>,
        slots: Arc<Slots>,
        reaches_of: ReachesOf,
        place: Place,
    ) -> Self {
        Lane {
            recipe,
            repo_path: repo_path.to_string(),
            services: services.to_vec(),
            at,
            memories,
            run_cap_mb,
            ended,
            slots,
            reaches_of,
            place,
            reached_by: vec![None; services.len()],
        }
    }

    /// The declared service `service`'s name in the run log.
    pub(super) fn label(&self, service: usize) -> &str {
        self.services[service]
            .service_name
            .as_deref()
            .unwrap_or("(root)")
    }

    /// What the walk of `service` reserves, the first time.
    pub(super) fn reservation_mb(&self, service: usize) -> u64 {
        self.memories[service].reservation_mb
    }

    /// The memory `service`'s walk starts with: its own, or the run's cap
    /// when it is walked `again`.
    fn memory(&self, service: usize, again: bool) -> ServiceMemory {
        if again {
            ServiceMemory::at_run_cap(self.run_cap_mb)
        } else {
            self.memories[service]
        }
    }

    /// Start `service`'s walk on a thread of its own.
    pub(super) fn start_walk(
        &mut self,
        service: usize,
        again: bool,
    ) -> futures::future::BoxFuture<'static, WalkDone> {
        let memory = self.memory(service, again);
        info!(
            "Walk {} started: heap cap {}, reservation {} MB",
            self.label(service),
            cap_text(memory.heap_cap_mb),
            memory.reservation_mb
        );
        let recipe = self
            .recipe
            .clone()
            .map(|recipe| recipe.with_heap_cap_mb(memory.heap_cap_mb));
        let (repo_path, config) = (self.repo_path.clone(), self.services[service].clone());
        let (at, place) = (self.at[service].clone(), Arc::clone(&self.place));
        let reaches_of = Arc::clone(&self.reaches_of);
        Box::pin(service_workers::on_a_thread(
            format!("carrick-walk-{}", service + 1),
            move |_| {
                let label = config.service_name.as_deref().unwrap_or("(root)");
                let span = tracing::info_span!("service", name = %label);
                let _entered = span.enter();
                let started = Instant::now();
                let deaths = MemoryDeathNote::default();
                let recipe = recipe.map(|recipe| recipe.noting_memory_deaths(&deaths));
                let walked = walk(recipe.as_ref(), &repo_path, &config, at.as_deref(), &place);
                let died = deaths.take().is_some();
                let reaches = if died || walked.rows.is_none() {
                    BTreeSet::new()
                } else {
                    reaches_of(service, &walked)
                };
                WalkDone {
                    walked,
                    died,
                    reaches,
                    secs: started.elapsed().as_secs_f64(),
                }
            },
        ))
    }

    /// `service`'s walk ended with `done`: walked again once at the run's
    /// cap when it died of memory below it, otherwise in its slot, its setup
    /// told.
    pub(super) fn walk_ended(&mut self, service: usize, again: bool, done: WalkDone) -> WalkEnd {
        let label = self.label(service).to_string();
        info!(
            "Walk {label} ended: {:.1} s, peak {}",
            done.secs,
            done.walked
                .peak_mb
                .map_or_else(|| "unknown".to_string(), |mb| format!("{mb} MB"))
        );
        let walked_at = self.memory(service, again).heap_cap_mb;
        if done.died && !again && walked_at != self.run_cap_mb {
            let alone = ServiceMemory::at_run_cap(self.run_cap_mb);
            info!(
                "Walk {label} died of memory at a heap cap of {}; walking it again alone at {}",
                cap_text(walked_at),
                cap_text(alone.heap_cap_mb)
            );
            return WalkEnd::Again {
                reservation_mb: alone.reservation_mb,
            };
        }
        if let Some(by) = self.reached_by[service] {
            info!(
                "Walked carried service {label} for {} in {:.1} s",
                self.label(by),
                done.secs
            );
        }
        self.slots.fill(service, done.walked);
        // Its setup hears this and is planned.
        drop(self.ended.get_mut(service).and_then(Option::take));
        WalkEnd::Walked {
            reaches: done.reaches,
        }
    }

    /// The carried `service`, first reached by the rows of `by`: filled with
    /// the rows an earlier pass kept when they are exactly what a walk would
    /// write now (carrick#3027), walked otherwise.
    pub(super) fn reached(&mut self, service: usize, by: usize) -> Reached {
        self.reached_by[service] = Some(by);
        let label = self.label(service).to_string();
        if let Some(commit) = &self.at[service] {
            match reuse(&self.repo_path, &self.services[service], commit) {
                Ok(walked) => {
                    debug!("Position rows for service {label} reused from the last pass");
                    let reaches = (self.reaches_of)(service, &walked);
                    self.slots.fill(service, walked);
                    return Reached::Filled { reaches };
                }
                Err(miss) => debug!("Position rows for service {label} not reused: {miss:?}"),
            }
        }
        if let Some(walking) = self.slots.walking.get(service) {
            walking.store(true, Ordering::SeqCst);
        }
        Reached::Walk {
            reservation_mb: self.memories[service].reservation_mb,
        }
    }
}

/// The files a walk of `service` sends, sorted, each under `scan_root`.
fn service_files(repo_path: &str, service: &Config, scan_root: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> =
        super::walk_service(repo_path, service, &super::service_ignore_patterns(service))
            .files
            .iter()
            .map(|file| scan_root.join(file.strip_prefix(repo_path).unwrap_or(file)))
            .collect();
    files.sort();
    files
}

/// The rows an earlier pass kept for `service`, carried at `commit`, as a
/// [`Walked`], when they are exactly what a walk would write now
/// ([`crate::position_rows::kept_rows`]); otherwise why not. Nothing is
/// spawned: a service this returns for is not walked (carrick#3027).
pub(super) fn reuse(
    repo_path: &str,
    service: &Config,
    commit: &str,
) -> Result<Walked, crate::position_rows::Miss> {
    let canonical = super::canonical_repo_path(repo_path);
    let scan_root = PathBuf::from(&canonical);
    let label = service
        .service_name
        .as_deref()
        .unwrap_or("(root)")
        .to_string();
    let kept = crate::position_rows::kept_path(
        &super::get_repository_name(&canonical),
        service.service_name.as_deref(),
    )
    .ok_or(crate::position_rows::Miss::NotKept)?;
    let files = service_files(repo_path, service, &scan_root);
    crate::position_rows::kept_rows(&kept, commit, &scan_root, &files)?;
    Ok(Walked {
        rows: Some(Destination::kept(kept)),
        scan_root,
        label,
        peak_mb: None,
    })
}

/// Walk one service's per-position compiler rows: written where `place`
/// says (beside its blob with a local store, under the OS temp dir
/// otherwise), named by the stem the blob will carry. The files are the ones
/// discovery walks; every path is sent under the repo's canonical root, as
/// the service's sidecar root is.
///
/// A run with no sidecar, or a walk that failed, keeps no rows. A walk that
/// fails says so in the run log, and nothing else.
fn walk(
    recipe: Option<&SidecarRecipe>,
    repo_path: &str,
    service: &Config,
    at: Option<&str>,
    place: &Place,
) -> Walked {
    let canonical = super::canonical_repo_path(repo_path);
    let scan_root = PathBuf::from(&canonical);
    let label = service
        .service_name
        .as_deref()
        .unwrap_or("(root)")
        .to_string();
    let no_rows = |scan_root: PathBuf, label: String| Walked {
        rows: None,
        scan_root,
        label,
        peak_mb: None,
    };
    let Some(out) = place(
        &super::get_repository_name(&canonical),
        service.service_name.as_deref(),
    ) else {
        return no_rows(scan_root, label);
    };
    let Some(recipe) = recipe else {
        return no_rows(scan_root, label);
    };
    let files = service_files(repo_path, service, &scan_root);
    let service_root = super::service_sidecar_root(repo_path, service);
    let walk = crate::position_rows::ServiceWalk {
        service_root: &service_root,
        tsconfig: service.tsconfig.as_deref(),
        scan_root: &scan_root,
        files: &files,
        out: out.path(),
        service: &label,
    };
    let peak_mb = match crate::position_rows::record_service_rows(recipe, &walk) {
        Ok((_, peak_mb)) => peak_mb,
        Err(error) => {
            debug!("Position rows for service {label} were not written: {error}");
            return no_rows(scan_root, label);
        }
    };
    crate::position_rows::record_walked_at(out.path(), at);
    Walked {
        rows: Some(out),
        scan_root,
        label,
        peak_mb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::services::type_sidecar::TypeSidecar;

    /// Rows written into `store`, as a scan with a local store writes them.
    fn into(store: &Path) -> Place {
        let store = store.to_path_buf();
        Arc::new(move |repo, service| {
            crate::position_rows::destination_from(
                Some("1"),
                Some(&store),
                &std::env::temp_dir(),
                repo,
                service,
            )
        })
    }

    /// With no sidecar to walk in there are no rows, and every lookup is a
    /// hole.
    #[test]
    fn a_walk_without_a_sidecar_keeps_no_rows() {
        let repo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let walked = walk(
            None,
            &repo.path().to_string_lossy(),
            &Config::default(),
            None,
            &into(store.path()),
        );
        assert!(walked.rows.is_none());
        assert_eq!(walked.index().file_count(), 0);
    }

    fn node_missing() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    /// A stand-in walk sidecar. It answers `init`, notes each `position_rows`
    /// request as `start <service> <heap cap or none> <ms>` in `walks`, does
    /// what `behaviour` says (it may die), then after the service's delay in
    /// `delays` writes the rows file, notes `end <service> <ms>` and answers.
    fn stand_in(dir: &Path, delays: &str, behaviour: &str) -> PathBuf {
        let script = dir.join("stand-in.cjs");
        std::fs::write(
            &script,
            format!(
                r#"
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const walks = path.join(__dirname, 'walks');
const flag = process.execArgv.find((arg) => arg.startsWith('--max-old-space-size='));
const heap = flag ? flag.split('=')[1] : 'none';
const delays = {delays};
const write = (frame) => process.stdout.write(JSON.stringify(frame) + '\n');
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  if (request.action === 'shutdown') process.exit(0);
  if (request.action === 'init') return write({{ request_id: request.request_id, status: 'ready' }});
  if (request.action !== 'position_rows') return;
  fs.appendFileSync(walks, `start ${{request.service}} ${{heap}} ${{Date.now()}}\n`);
  {behaviour}
  setTimeout(() => {{
    fs.mkdirSync(path.dirname(request.out_path), {{ recursive: true }});
    fs.writeFileSync(request.out_path, 'rows');
    fs.appendFileSync(walks, `end ${{request.service}} ${{Date.now()}}\n`);
    write({{
      request_id: request.request_id,
      status: 'success',
      result: {{ files: 1, rows: 1, resolved: 0, any: 0, error_type: 0, impl: 0, jsx: 0, bytes: 4, load_ms: 0, walk_ms: 0 }},
    }});
  }}, delays[request.service] ?? 0);
}});
"#
            ),
        )
        .unwrap();
        script
    }

    fn recipe_of(script: &Path) -> SidecarRecipe {
        let sidecar = TypeSidecar::spawn(script).expect("spawn the stand-in");
        let recipe = sidecar.recipe();
        drop(sidecar);
        recipe
    }

    /// A repo with one source file and `names` as its services.
    fn repo_of(root: &Path, names: &[&str]) -> (String, Vec<Config>) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.ts"), "export const a = 1;\n").unwrap();
        let services = names
            .iter()
            .map(|name| Config {
                service_name: Some(name.to_string()),
                ..Config::default()
            })
            .collect();
        (repo.to_string_lossy().into_owned(), services)
    }

    /// The `walks` lines, split on spaces.
    fn walks(dir: &Path) -> Vec<Vec<String>> {
        std::fs::read_to_string(dir.join("walks"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.split(' ').map(str::to_string).collect())
            .collect()
    }

    fn noted(walks: &[Vec<String>], what: &str, service: &str) -> Vec<Vec<String>> {
        walks
            .iter()
            .filter(|walk| walk[0] == what && walk[1] == service)
            .cloned()
            .collect()
    }

    /// When each service's [`WalkEnded`] was heard, in ms since the epoch.
    fn heard_at(walked: Vec<WalkEnded>) -> Vec<tokio::task::JoinHandle<u128>> {
        walked
            .into_iter()
            .map(|ended| {
                tokio::spawn(async move {
                    let _ = ended.await;
                    now_ms()
                })
            })
            .collect()
    }

    /// A pass of walks alone, through the admission the scan uses.
    struct WalksOnly(Lane);

    impl service_workers::WalkedPass for WalksOnly {
        type Walk = WalkDone;
        type Analysed = ();

        fn start_walk(
            &mut self,
            service: usize,
            again: bool,
        ) -> futures::future::BoxFuture<'static, WalkDone> {
            self.0.start_walk(service, again)
        }

        fn walk_ended(&mut self, service: usize, again: bool, walk: WalkDone) -> WalkEnd {
            self.0.walk_ended(service, again, walk)
        }

        fn reached(&mut self, service: usize, by: usize) -> Reached {
            self.0.reached(service, by)
        }

        fn start_analysis(
            &mut self,
            _: usize,
            _: &BTreeSet<usize>,
        ) -> futures::future::BoxFuture<'static, ()> {
            unreachable!("a pass of walks alone has no analysis")
        }
    }

    /// Walk `services` two at a time with no stored peaks under a run cap of
    /// 12 GB, rows into `store`; each one's slot once the pass has ended.
    async fn walk_two_at_a_time(
        script: &Path,
        repo: &str,
        services: &[Config],
        ends: Vec<oneshot::Sender<()>>,
        store: &Path,
    ) -> Arc<Slots> {
        let run_cap = Some(12_288);
        let slots = Arc::new(Slots::new(services.len()));
        let memories = vec![ServiceMemory::for_walk(None, run_cap); services.len()];
        let mut lane = WalksOnly(Lane::new(
            Some(recipe_of(script)),
            repo,
            services,
            vec![None; services.len()],
            memories,
            run_cap,
            ends.into_iter().map(Some).collect(),
            Arc::clone(&slots),
            Arc::new(|_, _| BTreeSet::new()),
            into(store),
        ));
        let walks: Vec<(usize, u64)> = (0..services.len())
            .map(|service| (service, lane.0.reservation_mb(service)))
            .collect();
        let plan = service_workers::Plan::count(2, "the test's");
        service_workers::walks_and_analyses(
            &service_workers::Ledger::new(),
            &plan,
            &walks,
            &[],
            &mut lane,
        )
        .await;
        service_workers::take_schedules();
        slots
    }

    /// carrick#3085: two walks run side by side at the walk's heap cap, and
    /// each service's setup hears its own walk end and no other.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_walks_overlap_and_each_setup_waits_for_its_own() {
        if node_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = stand_in(dir.path(), r#"{ "orders": 300, "billing": 2000 }"#, "");
        let (repo, services) = repo_of(dir.path(), &["orders", "billing"]);
        let store = tempfile::tempdir().unwrap();
        let (ends, walked) = signals(2);
        let heard = heard_at(walked);
        let slots = walk_two_at_a_time(&script, &repo, &services, ends, store.path()).await;
        let heard: Vec<u128> = futures::future::join_all(heard)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert!((0..2).all(|service| slots.get(service).unwrap().rows.is_some()));

        let walks = walks(dir.path());
        let ms = |line: &Vec<String>| line.last().unwrap().parse::<u128>().unwrap();
        let start = |service| ms(&noted(&walks, "start", service)[0]);
        let end = |service| ms(&noted(&walks, "end", service)[0]);
        // Each walk was started at the walk's cap, once.
        for service in ["orders", "billing"] {
            let started = noted(&walks, "start", service);
            assert_eq!(started.len(), 1, "{walks:?}");
            assert_eq!(
                started[0][2],
                service_workers::WALK_HEAP_CAP_MB.to_string(),
                "{walks:?}"
            );
        }
        // The spans overlap: each started before the other ended.
        assert!(start("billing") < end("orders"), "{walks:?}");
        assert!(start("orders") < end("billing"), "{walks:?}");
        // Each setup heard its own walk end, and the first did not wait for
        // the second's.
        assert!(heard[0] >= end("orders") && heard[0] < end("billing"));
        assert!(heard[1] >= end("billing"));
    }

    /// carrick#3085 (from #2563): a walk that dies of memory at the walk's
    /// cap is walked once more, alone and at the run's cap, after the walks
    /// queued with it; its rows are written and its setup waits for that
    /// walk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_walk_that_dies_of_memory_is_walked_again_at_the_run_cap() {
        if node_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = stand_in(
            dir.path(),
            r#"{ "orders": 0, "billing": 500 }"#,
            // V8's own way out, below the run's cap only.
            "if (request.service === 'orders' && heap !== '12288') { \
             fs.writeSync(2, 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\\n'); \
             process.abort(); }",
        );
        let (repo, services) = repo_of(dir.path(), &["orders", "billing"]);
        let store = tempfile::tempdir().unwrap();
        let (ends, walked) = signals(2);
        let heard = heard_at(walked);
        let slots = walk_two_at_a_time(&script, &repo, &services, ends, store.path()).await;
        let heard: Vec<u128> = futures::future::join_all(heard)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let walks = walks(dir.path());
        let heaps = |service| {
            noted(&walks, "start", service)
                .iter()
                .map(|line| line[2].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(heaps("orders"), ["6144", "12288"], "{walks:?}");
        assert_eq!(heaps("billing"), ["6144"], "{walks:?}");
        let rows = slots.get(0).unwrap().rows.as_ref();
        let rows = rows.expect("orders' rows were kept");
        assert!(rows.path().exists(), "orders' rows file was not written");
        assert!(slots.get(1).unwrap().rows.is_some());
        // Again only once the walk queued with it had ended.
        let ms = |line: &Vec<String>| line.last().unwrap().parse::<u128>().unwrap();
        let again = ms(&noted(&walks, "start", "orders")[1]);
        assert!(again >= ms(&noted(&walks, "end", "billing")[0]));
        // Its setup waited for the walk that wrote its rows.
        assert!(heard[0] >= ms(&noted(&walks, "end", "orders")[0]));
    }

    /// carrick#3085 (from #2563): the walk again is once. A walk that dies at
    /// the run's cap too keeps no rows, and is not walked a third time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_walk_that_dies_at_the_run_cap_too_is_not_walked_a_third_time() {
        if node_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = stand_in(
            dir.path(),
            "{}",
            "fs.writeSync(2, 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\\n'); \
             process.abort();",
        );
        let (repo, services) = repo_of(dir.path(), &["orders"]);
        let store = tempfile::tempdir().unwrap();
        let (ends, _walked) = signals(1);
        let slots = tokio::time::timeout(
            Duration::from_secs(60),
            walk_two_at_a_time(&script, &repo, &services, ends, store.path()),
        )
        .await
        .expect("the lane ended");
        assert!(slots.get(0).unwrap().rows.is_none());
        assert_eq!(walks(dir.path()).len(), 2, "{:?}", walks(dir.path()));
    }
}
