//! Every service's compiler walk, run before the analysis loop
//! (carrick#2413 slice G1).
//!
//! With `CARRICK_POSITION_ROWS=1` each service's per-position rows are
//! walked in a record-only sidecar of their own (carrick#2405). The walks
//! used to run inside the loop, one before each service's analysis. They
//! now run in a lane of their own as the scan starts, in declared order and
//! as many at once as the loop analyses, beside the queue that asks each
//! service's setup ahead ([`super::setup_ahead`]):
//!
//! * **Each setup waits for its own walk, and no other.** A service's plan is
//!   read once its walk has ended ([`WalkEnded`]), so the first service's
//!   setup is answered while the other walks run. Asking a setup from inside
//!   the loop instead, after that service's walk, undoes carrick#1895: the
//!   loop would wait for every setup again.
//! * **The loop starts once the lane has ended.** No walk process is alive
//!   beside an analysis sidecar, so the two programs never share the
//!   machine's memory.
//! * **The rows are kept until the run ends** ([`Walked`]): read by the
//!   service's analysis and again by the owed-work retry, from one walk.
//!
//! * **The lane has its own width** (carrick#2560): a walk is sized by a
//!   walk's memory ([`WALK_HEAP_CAP_MB`]), not an analysis sidecar's, so a
//!   runner that holds one analysis sidecar still walks several services at
//!   once ([`service_workers::walk_width`]). A walk that dies of memory at
//!   that cap is walked again, alone, at the run's full cap once the lane
//!   has ended, and its setup waits for that walk.
//!
//! Off, there is no lane: the queue and the loop run as they did.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;
use tracing::debug;

use super::service_workers;
use crate::config::Config;
use crate::position_rows::{Destination, PositionIndex};
use crate::services::type_sidecar::{MemoryDeathNote, SidecarRecipe};

/// The V8 old-space cap a walk's sidecar is started with, in MB, unless the
/// run's own cap is lower (carrick#2560). Measured on a 35-service monorepo,
/// the largest walk peaked at 3.9 GB and most under 1 GB.
pub(super) const WALK_HEAP_CAP_MB: u64 = 6144;

/// The heap caps the lane's walks are started with: `walk` for the lane,
/// `full` (the run's) for a walk that died of memory at `walk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WalkCaps {
    pub walk: Option<u64>,
    pub full: Option<u64>,
}

impl WalkCaps {
    /// [`WALK_HEAP_CAP_MB`], or the run's cap when that is lower. A run with
    /// the cap turned off walks with none either.
    pub(super) fn for_run(full: Option<u64>) -> Self {
        Self {
            walk: full.map(|full| full.min(WALK_HEAP_CAP_MB)),
            full,
        }
    }

    /// Whether a walk that died of memory has a larger cap to be walked
    /// again at.
    fn retries(&self) -> bool {
        self.walk != self.full
    }
}

/// Where a service's rows go this run, from the repo name and the service
/// name: [`crate::position_rows::destination`] in a scan.
type Place = Arc<dyn Fn(&str, Option<&str>) -> Option<Destination> + Send + Sync>;

/// Heard by a service's setup once its walk has ended, however it ended:
/// a walk that failed or panicked drops its sender, which is heard the same.
pub(super) type WalkEnded = oneshot::Receiver<()>;

/// One service's walk, as the run keeps it until it ends.
#[derive(Debug)]
pub(super) struct Walked {
    /// The rows file, when the walk wrote one. A temporary one is removed
    /// when this is dropped, which is when the run ends.
    rows: Option<Destination>,
    scan_root: PathBuf,
    label: String,
}

impl Walked {
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

/// Whether this run walks the rows at all.
pub(super) fn requested() -> bool {
    std::env::var(crate::position_rows::POSITION_ROWS_ENV).as_deref() == Ok("1")
}

/// One heard [`WalkEnded`] per service, and the senders the lane drops as
/// each walk ends.
pub(super) fn signals(count: usize) -> (Vec<oneshot::Sender<()>>, Vec<WalkEnded>) {
    (0..count).map(|_| oneshot::channel()).unzip()
}

/// Walk `services` in declared order, `width` at a time, each on a thread of
/// its own and at `caps.walk`, and return each one's [`Walked`] at its
/// position. Each service's sender in `ended` is dropped as its walk ends.
///
/// A walk that died of memory is walked again once the lane has ended, one
/// at a time, at `caps.full`: its sender is held until that walk ends, so
/// its setup waits for the rows it will read.
pub(super) async fn walk_all(
    recipe: Option<SidecarRecipe>,
    repo_path: &str,
    services: &[Config],
    width: usize,
    caps: WalkCaps,
    ended: Vec<oneshot::Sender<()>>,
) -> Vec<Walked> {
    let place: Place = Arc::new(crate::position_rows::destination);
    walk_all_into(recipe, repo_path, services, width, caps, ended, place).await
}

/// [`walk_all`], each service's rows written where `place` says.
async fn walk_all_into(
    recipe: Option<SidecarRecipe>,
    repo_path: &str,
    services: &[Config],
    width: usize,
    caps: WalkCaps,
    ended: Vec<oneshot::Sender<()>>,
    place: Place,
) -> Vec<Walked> {
    let mut ended: Vec<Option<oneshot::Sender<()>>> = ended.into_iter().map(Some).collect();
    ended.resize_with(services.len(), || None);
    let lane = service_workers::admit_in_declared_order(services.len(), width, |position| {
        let (repo_path, place) = (repo_path.to_string(), place.clone());
        let recipe = recipe
            .clone()
            .map(|recipe| recipe.with_heap_cap_mb(caps.walk));
        let service = services[position].clone();
        let ended = ended[position].take();
        service_workers::on_a_thread(format!("carrick-walk-{}", position + 1), move |_| {
            let label = service.service_name.as_deref().unwrap_or("(root)");
            let span = tracing::info_span!("service", name = %label);
            let _entered = span.enter();
            let deaths = MemoryDeathNote::default();
            let recipe = recipe.map(|recipe| recipe.noting_memory_deaths(&deaths));
            let walked = walk(recipe.as_ref(), &repo_path, &service, &place);
            // Dropped here when the walk ends, whichever way, so the setup
            // waiting for it hears that and is planned; held for the walk
            // again when this one died of memory below the run's cap.
            let again = (deaths.take().is_some() && caps.retries()).then_some(ended);
            (walked, again)
        })
    })
    .await;
    let mut walks = Vec::with_capacity(lane.len());
    for (position, (walked, again)) in lane.into_iter().enumerate() {
        let Some(ended) = again else {
            walks.push(walked);
            continue;
        };
        let (repo_path, place) = (repo_path.to_string(), place.clone());
        let recipe = recipe
            .clone()
            .map(|recipe| recipe.with_heap_cap_mb(caps.full));
        let service = services[position].clone();
        let walked =
            service_workers::on_a_thread(format!("carrick-walk-{}", position + 1), move |_| {
                let label = service.service_name.as_deref().unwrap_or("(root)");
                let span = tracing::info_span!("service", name = %label);
                let _entered = span.enter();
                debug!(
                    "Position rows for service {label}: the walk died of memory at a {} MB heap; \
                     walking it again alone at {}",
                    caps.walk.unwrap_or_default(),
                    caps.full
                        .map_or_else(|| "V8's own heap".to_string(), |mb| format!("{mb} MB")),
                );
                walk(recipe.as_ref(), &repo_path, &service, &place)
            })
            .await;
        drop(ended);
        walks.push(walked);
    }
    walks
}

/// Walk one service's per-position compiler rows: written beside its blob
/// with a local store, under the OS temp dir otherwise, and named by the
/// stem the blob will carry. The files are the ones discovery walks; every
/// path is sent under the repo's canonical root, as the service's sidecar
/// root is.
///
/// A run with no sidecar, or a walk that failed, keeps no rows. A walk that
/// fails says so in the run log, and nothing else.
fn walk(
    recipe: Option<&SidecarRecipe>,
    repo_path: &str,
    service: &Config,
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
    let mut files: Vec<PathBuf> =
        super::walk_service(repo_path, service, &super::service_ignore_patterns(service))
            .files
            .iter()
            .map(|file| scan_root.join(file.strip_prefix(repo_path).unwrap_or(file)))
            .collect();
    files.sort();
    let service_root = super::service_sidecar_root(repo_path, service);
    let walk = crate::position_rows::ServiceWalk {
        service_root: &service_root,
        tsconfig: service.tsconfig.as_deref(),
        scan_root: &scan_root,
        files: &files,
        out: out.path(),
        service: &label,
    };
    if let Err(error) = crate::position_rows::record_service_rows(recipe, &walk) {
        debug!("Position rows for service {label} were not written: {error}");
        return no_rows(scan_root, label);
    }
    Walked {
        rows: Some(out),
        scan_root,
        label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            &into(store.path()),
        );
        assert!(walked.rows.is_none());
        assert_eq!(walked.index().file_count(), 0);
    }

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

    /// carrick#2560: the lane is sized by a walk's memory. On a 4-core
    /// runner with 18,574 MB free the analysis holds one service at its
    /// 12 GB heap, and the lane walks two.
    #[test]
    fn the_lane_is_sized_by_a_walks_memory_not_an_analysis_sidecars() {
        let lane = service_workers::walk_width_for(35, Some(18_574), 4, None);
        assert_eq!(lane.count, 2, "{}", lane.why);
        let analysis = service_workers::width_for(35, false, Some(18_574), 4, Some(12_288), None);
        assert_eq!(analysis.count, 1, "{}", analysis.why);
        // 2 cores and 4 GB free: one at a time.
        assert_eq!(
            service_workers::walk_width_for(35, Some(4 * 1024), 2, None).count,
            1
        );
        // The knob forces the lane as it forces the analysis.
        assert_eq!(
            service_workers::walk_width_for(35, Some(18_574), 4, Some(1)).count,
            1
        );
        // Never more walks than services.
        assert_eq!(
            service_workers::walk_width_for(1, Some(64 * 1024), 16, None).count,
            1
        );
    }

    /// carrick#2560: walks run below the run's cap, never above it, and a
    /// run with the cap turned off walks with none.
    #[test]
    fn a_walk_is_capped_below_the_run_never_above_it() {
        assert_eq!(
            WalkCaps::for_run(Some(12_288)),
            WalkCaps {
                walk: Some(WALK_HEAP_CAP_MB),
                full: Some(12_288)
            }
        );
        assert!(WalkCaps::for_run(Some(12_288)).retries());
        assert_eq!(WalkCaps::for_run(Some(4096)).walk, Some(4096));
        assert!(!WalkCaps::for_run(Some(4096)).retries());
        assert_eq!(WalkCaps::for_run(None).walk, None);
        assert!(!WalkCaps::for_run(None).retries());
    }

    /// carrick#2560: two walks run side by side at the walk's heap cap, and
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
        let walks_out = walk_all_into(
            Some(recipe_of(&script)),
            &repo,
            &services,
            2,
            WalkCaps::for_run(Some(12_288)),
            ends,
            into(store.path()),
        )
        .await;
        let heard: Vec<u128> = futures::future::join_all(heard)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert!(walks_out.iter().all(|walked| walked.rows.is_some()));

        let walks = walks(dir.path());
        let ms = |line: &Vec<String>| line.last().unwrap().parse::<u128>().unwrap();
        let start = |service| ms(&noted(&walks, "start", service)[0]);
        let end = |service| ms(&noted(&walks, "end", service)[0]);
        // Each walk was started at the walk's cap, once.
        for service in ["orders", "billing"] {
            let started = noted(&walks, "start", service);
            assert_eq!(started.len(), 1, "{walks:?}");
            assert_eq!(started[0][2], WALK_HEAP_CAP_MB.to_string(), "{walks:?}");
        }
        // The spans overlap: each started before the other ended.
        assert!(start("billing") < end("orders"), "{walks:?}");
        assert!(start("orders") < end("billing"), "{walks:?}");
        // Each setup heard its own walk end, and the first did not wait for
        // the second's.
        assert!(heard[0] >= end("orders") && heard[0] < end("billing"));
        assert!(heard[1] >= end("billing"));
    }

    /// carrick#2560: a walk that dies of memory at the walk's cap is walked
    /// once more, alone and at the run's full cap, after the lane; its rows
    /// are written and its setup waits for that walk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_walk_that_dies_of_memory_is_walked_again_at_the_full_cap() {
        if node_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = stand_in(
            dir.path(),
            r#"{ "orders": 0, "billing": 500 }"#,
            // V8's own way out, below the full cap only.
            "if (request.service === 'orders' && heap !== '12288') { \
             fs.writeSync(2, 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\\n'); \
             process.abort(); }",
        );
        let (repo, services) = repo_of(dir.path(), &["orders", "billing"]);
        let store = tempfile::tempdir().unwrap();
        let (ends, walked) = signals(2);
        let heard = heard_at(walked);
        let walks_out = walk_all_into(
            Some(recipe_of(&script)),
            &repo,
            &services,
            2,
            WalkCaps::for_run(Some(12_288)),
            ends,
            into(store.path()),
        )
        .await;
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
        let rows = walks_out[0].rows.as_ref().expect("orders' rows were kept");
        assert!(rows.path().exists(), "orders' rows file was not written");
        assert!(walks_out[1].rows.is_some());
        // Again only once the lane had ended: after the other walk.
        let ms = |line: &Vec<String>| line.last().unwrap().parse::<u128>().unwrap();
        let again = ms(&noted(&walks, "start", "orders")[1]);
        assert!(again >= ms(&noted(&walks, "end", "billing")[0]));
        // Its setup waited for the walk that wrote its rows.
        assert!(heard[0] >= ms(&noted(&walks, "end", "orders")[0]));
    }

    /// carrick#2560: the walk again is once. A walk that dies at the full cap
    /// too keeps no rows, and is not walked a third time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_walk_that_dies_at_the_full_cap_too_is_not_walked_a_third_time() {
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
        let walks_out = tokio::time::timeout(
            Duration::from_secs(60),
            walk_all_into(
                Some(recipe_of(&script)),
                &repo,
                &services,
                2,
                WalkCaps::for_run(Some(12_288)),
                ends,
                into(store.path()),
            ),
        )
        .await
        .expect("the lane ended");
        assert!(walks_out[0].rows.is_none());
        assert_eq!(walks(dir.path()).len(), 2, "{:?}", walks(dir.path()));
    }
}
