//! Several sidecar processes scoped to one service, answering one pass's work
//! at once (carrick#1996, carrick#1993).
//!
//! A sidecar handles one request at a time, and some passes are a long run of
//! independent requests over one service's program: the retype check rebuilds
//! a type checker for every call it judges, and the signature pass pays the
//! first look at every file. Separate processes can do that work at once. Each
//! one builds the service's program for itself, so the number of processes is
//! bounded by the memory the machine has free.
//!
//! The pool changes when and where a job runs, never what it is asked: a
//! caller hands it the same requests it would send to one process, and gets
//! the answers back in the order it gave the jobs.
//!
//! - [`pool_size`] decides how many processes a pool may run.
//! - [`SidecarPool::scoped`] borrows the caller's own sidecar, already scoped
//!   to the service, and starts the others beside it.
//! - [`SidecarPool::run`] answers a list of jobs, each on whichever process
//!   is free next.
//! - [`jobs_by_file`] cuts a list of items into jobs that keep each file's
//!   items together, in their order.
//!
//! Its two users land after it: the retype check (carrick#1996) and the
//! signature pass (carrick#1993). The `dead_code` allowance below is for the
//! binary until then, and goes with the first of them.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::{debug, warn};

use crate::services::type_sidecar::{SidecarError, TypeSidecar, ready_budget};

/// Env override for the number of processes a pool runs, the caller's own
/// included. `1` turns pools off; a number above what the machine can hold is
/// still capped by memory. Unset, or `0`, sizes the pool from the machine.
pub const POOL_ENV: &str = "CARRICK_SIDECAR_POOL";

/// The most processes one pool runs, whatever the machine holds: past a few,
/// the processes compete for the same memory bandwidth and disk.
const MAX_PROCESSES: usize = 4;

/// Memory left free for everything else on the machine (the scanner, the
/// editor and browser of whoever runs the scan), never handed to a pool.
const HEADROOM_MB: u64 = 2048;

/// How many processes a pool runs, and why, for the scan's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSize {
    /// The caller's own process included, so never below one.
    pub processes: usize,
    pub why: String,
}

/// How many processes a pool may run for work whose processes each take
/// about `per_process_mb`: the caller's own, and as many more as the free
/// memory holds after [`HEADROOM_MB`], no more than one per spare core and
/// [`MAX_PROCESSES`] in all. [`POOL_ENV`] sets the number instead, still
/// capped by memory.
pub fn pool_size(per_process_mb: u64) -> PoolSize {
    let forced = std::env::var(POOL_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|n| *n > 0);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    size_for(available_memory_mb(), per_process_mb, cores, forced)
}

/// [`pool_size`] with its inputs given.
fn size_for(
    available_mb: Option<u64>,
    per_process_mb: u64,
    cores: usize,
    forced: Option<usize>,
) -> PoolSize {
    let per = per_process_mb.max(1);
    let Some(available) = available_mb else {
        return PoolSize {
            processes: 1,
            why: "one process: the free memory could not be read".to_string(),
        };
    };
    let by_memory = 1 + (available.saturating_sub(HEADROOM_MB) / per) as usize;
    let by_cores = cores.saturating_sub(1).max(1);
    let wanted = forced.unwrap_or_else(|| by_cores.min(MAX_PROCESSES));
    let processes = wanted.min(by_memory).max(1);
    let why = format!(
        "{processes} process(es): {wanted} {} and {available} MB free holds {by_memory} at {per} MB each after {HEADROOM_MB} MB kept back",
        if forced.is_some() {
            format!("asked for by {POOL_ENV}")
        } else {
            format!("allowed by {cores} core(s)")
        },
    );
    PoolSize { processes, why }
}

/// Memory the machine can hand out now, in MB: what is free plus what the
/// system would reclaim first. `None` when it cannot be read.
fn available_memory_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemAvailable:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        let available = kb / 1024;
        // Inside a container the host's figure is not ours to spend.
        let cgroup = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .ok()
            .and_then(|max| max.trim().parse::<u64>().ok())
            .zip(
                std::fs::read_to_string("/sys/fs/cgroup/memory.current")
                    .ok()
                    .and_then(|used| used.trim().parse::<u64>().ok()),
            )
            .map(|(max, used)| max.saturating_sub(used) / (1024 * 1024));
        return Some(cgroup.map_or(available, |left| left.min(available)));
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("vm_stat").output().ok()?;
        return vm_stat_available_mb(&String::from_utf8_lossy(&out.stdout));
    }
    #[allow(unreachable_code)]
    None
}

/// Free, inactive and speculative pages from `vm_stat`'s output, in MB.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn vm_stat_available_mb(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(label))?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let total = pages("Pages free:")? + pages("Pages inactive:")? + pages("Pages speculative:")?;
    Some(total * page / (1024 * 1024))
}

/// Resident memory of `sidecar`'s process in MB, from `ps`: an estimate of
/// what another process scoped to the same service takes once it has built
/// the program, for [`pool_size`]. `None` when it cannot be read. Read after
/// the process has built its program, or it says nothing about the program.
pub fn resident_mb(sidecar: &TypeSidecar) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &sidecar.pid().to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb / 1024)
}

/// The caller's sidecar and the processes started beside it, each scoped to
/// the same service.
pub struct SidecarPool<'a> {
    base: &'a TypeSidecar,
    extra: Vec<TypeSidecar>,
}

impl<'a> SidecarPool<'a> {
    /// A pool of up to `processes` processes for the service at `root`:
    /// `base`, scoped to it unless it already is, and up to `processes - 1`
    /// more started from the same sidecar script with `base`'s scope copied
    /// exactly (root, tsconfig and scan root) and its operation deadline. A process
    /// that does not start or does not become ready is left out and logged;
    /// the pool always holds `base`. An error only when `base` cannot be
    /// scoped, as a caller with no pool would have met it.
    pub fn scoped(
        base: &'a TypeSidecar,
        root: &Path,
        tsconfig: Option<&str>,
        processes: usize,
    ) -> Result<Self, SidecarError> {
        if !base.is_scoped_to(root, tsconfig) {
            base.start_init(root, tsconfig);
            base.wait_ready(ready_budget())?;
        }
        let mut extra = Vec::new();
        for _ in 1..processes.max(1) {
            match base.spawn_scoped_like(root, tsconfig) {
                Ok(sidecar) => extra.push(sidecar),
                Err(e) => {
                    warn!("A pool process for {} did not start: {e}", root.display());
                    break;
                }
            }
        }
        debug!(
            "Sidecar pool for {}: {} process(es)",
            root.display(),
            extra.len() + 1
        );
        Ok(Self { base, extra })
    }

    /// How many processes the pool runs, `base` included.
    pub fn processes(&self) -> usize {
        self.extra.len() + 1
    }

    /// Answer every job: `work` runs each job on whichever process takes it
    /// next, one job per process at a time, and the answers come back in the
    /// order of `jobs`. A job's failure is its own answer, so `work` maps
    /// errors into `R`. With one process this is `jobs.iter().map(...)` on
    /// `base`, in order.
    pub fn run<J, R, F>(&self, jobs: &[J], work: F) -> Vec<R>
    where
        J: Sync,
        R: Send,
        F: Fn(&TypeSidecar, &J) -> R + Sync,
    {
        if self.extra.is_empty() || jobs.len() <= 1 {
            return jobs.iter().map(|job| work(self.base, job)).collect();
        }
        let next = AtomicUsize::new(0);
        let answers: Vec<Mutex<Option<R>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
        let workers: Vec<&TypeSidecar> = std::iter::once(self.base)
            .chain(self.extra.iter())
            .collect();
        std::thread::scope(|scope| {
            for sidecar in workers {
                let (next, answers, work) = (&next, &answers, &work);
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        let Some(job) = jobs.get(index) else {
                            break;
                        };
                        let answer = work(sidecar, job);
                        *answers[index].lock().unwrap_or_else(|p| p.into_inner()) = Some(answer);
                    }
                });
            }
        });
        answers
            .into_iter()
            .map(|slot| {
                slot.into_inner()
                    .unwrap_or_else(|p| p.into_inner())
                    .expect("every job is taken by exactly one process")
            })
            .collect()
    }
}

impl Drop for SidecarPool<'_> {
    fn drop(&mut self) {
        for sidecar in &self.extra {
            let _ = sidecar.shutdown();
        }
    }
}

/// Cut `items` into jobs of whole files. Items are grouped by `file_of`, each
/// file's items in the order they came and the files in the order each first
/// appears. A job holds as many whole files as fit in `max` items, so one
/// process pays a file's first look once; a file with more than `max` items
/// is a job of its own, whole. A file is never split across two jobs.
pub fn jobs_by_file<T: Clone>(
    items: &[T],
    file_of: impl Fn(&T) -> &str,
    max: usize,
) -> Vec<Vec<T>> {
    let mut files: Vec<(&str, Vec<T>)> = Vec::new();
    for item in items {
        let file = file_of(item);
        match files.iter_mut().find(|(seen, _)| *seen == file) {
            Some((_, group)) => group.push(item.clone()),
            None => files.push((file, vec![item.clone()])),
        }
    }
    let mut jobs: Vec<Vec<T>> = Vec::new();
    let mut current: Vec<T> = Vec::new();
    for (_, group) in files {
        if !current.is_empty() && current.len() + group.len() > max {
            jobs.push(std::mem::take(&mut current));
        }
        current.extend(group);
    }
    if !current.is_empty() {
        jobs.push(current);
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_is_sized_by_free_memory_cores_and_the_cap() {
        let size =
            |available, per, cores, forced| size_for(available, per, cores, forced).processes;
        // Memory unknown: one process.
        assert_eq!(size(None, 3000, 8, None), 1);
        // 10 GB free, 3 GB a process after 2 GB kept back: the base and two more.
        assert_eq!(size(Some(10_240), 3000, 8, None), 3);
        // Plenty of memory: one per spare core, at most four.
        assert_eq!(size(Some(64_000), 1000, 3, None), 2);
        assert_eq!(size(Some(64_000), 1000, 16, None), MAX_PROCESSES);
        // One core, or too little memory: one process, never zero.
        assert_eq!(size(Some(64_000), 1000, 1, None), 1);
        assert_eq!(size(Some(1_000), 3000, 8, None), 1);
        // The override sets the count, still capped by memory.
        assert_eq!(size(Some(64_000), 1000, 2, Some(6)), 6);
        assert_eq!(size(Some(10_240), 3000, 8, Some(6)), 3);
        assert_eq!(size(Some(64_000), 1000, 8, Some(1)), 1);
        let why = size_for(Some(10_240), 3000, 8, None).why;
        assert!(why.starts_with("3 process(es)"), "{why}");
    }

    #[test]
    fn vm_stat_reads_free_inactive_and_speculative_pages() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                    Pages free:                               65536.\n\
                    Pages active:                            900000.\n\
                    Pages inactive:                          131072.\n\
                    Pages speculative:                         1024.\n";
        // (65536 + 131072 + 1024) * 16 KB = 3088 MB.
        assert_eq!(vm_stat_available_mb(text), Some(3088));
        assert_eq!(vm_stat_available_mb("no header"), None);
    }

    #[test]
    fn jobs_hold_whole_files_in_order_and_never_split_one() {
        let items = [
            ("a", 1),
            ("b", 1),
            ("a", 2),
            ("c", 1),
            ("b", 2),
            ("a", 3),
            ("d", 1),
        ];
        let jobs = jobs_by_file(&items, |(file, _)| file, 4);
        assert_eq!(
            jobs,
            vec![
                vec![("a", 1), ("a", 2), ("a", 3)],
                vec![("b", 1), ("b", 2), ("c", 1), ("d", 1)],
            ]
        );
        // A file larger than a job is a job of its own, whole.
        let big: Vec<(&str, i32)> = [("b", 1)]
            .into_iter()
            .chain((1..=5).map(|n| ("a", n)))
            .chain([("c", 1)])
            .collect();
        let jobs = jobs_by_file(&big, |(file, _)| file, 2);
        assert_eq!(
            jobs,
            vec![
                vec![("b", 1)],
                vec![("a", 1), ("a", 2), ("a", 3), ("a", 4), ("a", 5)],
                vec![("c", 1)],
            ]
        );
        assert!(jobs_by_file::<(&str, i32)>(&[], |(file, _)| file, 3).is_empty());
    }

    /// A stand-in sidecar that answers each retype item with its own pid as
    /// the reason, after a short wait so that every process gets work.
    fn stand_in(dir: &Path) -> std::path::PathBuf {
        let script = dir.join("stand-in-sidecar.cjs");
        std::fs::write(
            &script,
            r#"
const fs = require('fs');
const write = (frame) => fs.writeSync(1, JSON.stringify(frame) + '\n');
require('readline').createInterface({ input: process.stdin, terminal: false }).on('line', (line) => {
  const request = JSON.parse(line);
  const request_id = request.request_id;
  if (request.action === 'shutdown') { write({ request_id, status: 'success' }); process.exit(0); }
  if (request.action === 'init') return write({ request_id, status: 'ready' });
  const until = Date.now() + 150;
  while (Date.now() < until) {}
  write({
    request_id,
    status: 'success',
    outcomes: request.items.map((item) => ({ item_id: item.item_id, outcome: 'abstain', diagnostics: [], reason: String(process.pid) })),
  });
});
"#,
        )
        .unwrap();
        script
    }

    fn item(id: &str) -> crate::services::type_sidecar::RetypeItem {
        crate::services::type_sidecar::RetypeItem {
            item_id: id.to_string(),
            file_path: "/repo/src/a.ts".to_string(),
            line_number: 1,
            span_start: None,
            span_end: None,
            expression_text: None,
            expression_line: None,
            producer_type: "{ id: string; }".to_string(),
            producer_unwidened_type: None,
            wire: true,
        }
    }

    /// Jobs are answered by every process of the pool, and the answers come
    /// back in the order of the jobs whichever process ran them. The extra
    /// processes are scoped to the same root as the caller's.
    #[test]
    fn a_pool_answers_every_job_in_order_across_its_processes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = TypeSidecar::spawn(&stand_in(&root)).unwrap();
        base.start_init(&root, None);
        base.wait_ready(std::time::Duration::from_secs(20)).unwrap();

        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);
        let jobs: Vec<String> = (0..12).map(|n| format!("job-{n}")).collect();
        let answers = pool.run(&jobs, |sidecar, job| {
            assert!(sidecar.is_scoped_to(&root, None));
            let outcome = sidecar
                .retype_check(&[item(job)])
                .expect("the stand-in answers")
                .remove(0);
            (outcome.item_id, outcome.reason.unwrap())
        });
        assert_eq!(
            answers.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            jobs,
            "answers in the order of the jobs"
        );
        let pids: std::collections::BTreeSet<&String> =
            answers.iter().map(|(_, pid)| pid).collect();
        assert_eq!(
            pids.len(),
            3,
            "every process answered some job: {answers:?}"
        );
        assert!(
            pids.contains(&base.pid().to_string()),
            "the caller's own process is one of them"
        );
        assert!(resident_mb(&base).is_some_and(|mb| mb > 0));

        // One process: the caller's own, in order.
        let single = SidecarPool::scoped(&base, &root, None, 1).unwrap();
        assert_eq!(single.processes(), 1);
        let answers = single.run(&jobs, |sidecar, job| {
            sidecar
                .retype_check(&[item(job)])
                .unwrap()
                .remove(0)
                .reason
                .unwrap()
        });
        assert!(answers.iter().all(|pid| pid == &answers[0]));
    }
}
