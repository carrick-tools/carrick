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
//!   to the service, and starts the others beside it, each to be given the
//!   files of the caller's program so that every process holds the same
//!   program.
//! - [`SidecarPool::run`] answers a list of jobs, each on whichever process
//!   is free next; a started process joins once it has built the program. A process whose answer says it is gone takes no more; the
//!   others take the rest, and with none left the rest are not sent.
//! - [`jobs_by_file`] cuts a list of items into jobs that keep each file's
//!   items together, in their order.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

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

/// Resident memory of `sidecar`'s process in MB: an estimate of what another
/// process scoped to the same service takes once it has built the program,
/// for [`pool_size`]. `None` when it cannot be read. Read after the process
/// has built its program, or it says nothing about the program.
///
/// Asked of the kernel, not of `ps`: `ps` is setuid on macOS, so a sandbox
/// that refuses setuid binaries refuses it, and a slim container may not
/// have it. Without the size no pool starts.
pub fn resident_mb(sidecar: &TypeSidecar) -> Option<u64> {
    resident_bytes(sidecar.pid()).map(|bytes| bytes / (1024 * 1024))
}

/// The resident size of process `pid` in bytes, from `proc_pidinfo`.
#[cfg(target_os = "macos")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: a zeroed `proc_taskinfo` is a valid value of that plain C struct.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `proc_pidinfo` writes at most `size` bytes, the size of `info`.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    (written == size).then_some(info.pti_resident_size)
}

/// The resident size of process `pid` in bytes, from `/proc/<pid>/status`.
#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// The caller's sidecar and the processes started beside it, each scoped to
/// the same service.
pub struct SidecarPool<'a> {
    base: &'a TypeSidecar,
    extra: Vec<TypeSidecar>,
    /// The files `base`'s program was built from, in order, that each of
    /// `extra` is given before its first job.
    program: Vec<PathBuf>,
    /// Whether each of `extra` holds `base`'s program: set once, by its first
    /// [`SidecarPool::run`].
    holds_program: Vec<OnceLock<bool>>,
    /// Each of `extra` that was stopped while it built the program, because
    /// no job was left for it.
    stopped: Vec<AtomicBool>,
}

impl<'a> SidecarPool<'a> {
    /// A pool of up to `processes` processes for the service at `root`:
    /// `base`, scoped to it unless it already is, and up to `processes - 1`
    /// more started from the same sidecar script with `base`'s scope copied
    /// exactly (root, tsconfig and scan root) and its operation deadline.
    ///
    /// Each process started is also given `base`'s program before any job:
    /// the files `base`'s program was built from, in order, read here
    /// ([`TypeSidecar::program_files`], carrick#2027), so that it builds the
    /// same program. Without them a process builds from its tsconfig alone
    /// and adds a file when it is first asked about it, so two processes can
    /// hold their files in different orders, and the compiler orders two
    /// types of the same name by that order. The copy is made by
    /// [`SidecarPool::run`], so `base` is not kept waiting for it.
    ///
    /// A process that does not start or does not become ready is left out
    /// and logged; when `base`'s files cannot be read, none is started. The
    /// pool always holds `base`. An error only when `base` cannot be scoped,
    /// as a caller with no pool would have met it.
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
        let alone = |base: &'a TypeSidecar| Self {
            base,
            extra: Vec::new(),
            program: Vec::new(),
            holds_program: Vec::new(),
            stopped: Vec::new(),
        };
        if processes <= 1 {
            return Ok(alone(base));
        }
        let program = match base.program_files() {
            Ok(files) => files,
            Err(e) => {
                warn!(
                    "No pool process for {} was started: the files of the program the others would copy could not be read: {e}",
                    root.display()
                );
                return Ok(alone(base));
            }
        };
        let mut extra = Vec::new();
        for _ in 1..processes {
            match base.spawn_scoped_like(root, tsconfig) {
                Ok(sidecar) => extra.push(sidecar),
                Err(e) => {
                    warn!("A pool process for {} did not start: {e}", root.display());
                    break;
                }
            }
        }
        debug!(
            "Sidecar pool for {}: {} process(es), each to build from {} file(s)",
            root.display(),
            extra.len() + 1,
            program.len()
        );
        let holds_program = extra.iter().map(|_| OnceLock::new()).collect();
        let stopped = extra.iter().map(|_| AtomicBool::new(false)).collect();
        Ok(Self {
            base,
            extra,
            program,
            holds_program,
            stopped,
        })
    }

    /// Whether started process `index` holds `base`'s program, giving it the
    /// program's files the first time this is asked. A process that cannot
    /// take them takes no jobs.
    fn holds_program(&self, index: usize) -> bool {
        *self.holds_program[index].get_or_init(|| {
            let sidecar = &self.extra[index];
            match sidecar.add_program_files(&self.program) {
                Ok(_) => true,
                Err(_) if self.stopped[index].load(Ordering::SeqCst) => {
                    debug!(
                        "A sidecar pool process (pid {}) was stopped while it built the program: no job was left for it",
                        sidecar.pid()
                    );
                    false
                }
                Err(e) => {
                    warn!(
                        "A sidecar pool process (pid {}) could not build the program the others hold, and takes no jobs: {e}",
                        sidecar.pid()
                    );
                    false
                }
            }
        })
    }

    /// Stop every started process that is still building `base`'s program,
    /// once no job is left: it would take none, and the run would otherwise
    /// wait for its build before it returns.
    fn stop_the_unready(&self) {
        for (index, sidecar) in self.extra.iter().enumerate() {
            if self.holds_program[index].get().is_none() {
                self.stopped[index].store(true, Ordering::SeqCst);
                sidecar.stop();
            }
        }
    }

    /// How many processes the pool runs, `base` included.
    pub fn processes(&self) -> usize {
        self.extra.len() + 1
    }

    /// Answer every job a process is left to take: `work` runs each job on
    /// whichever process takes it next, one job per process at a time, and
    /// the answers come back in the order of `jobs`. A job's failure is its
    /// own answer, so `work` maps errors into `R`.
    ///
    /// `leaves_no_process` says which answers mean the process that gave it
    /// is gone (for an error, [`SidecarError::leaves_no_process`]; a timeout
    /// is not one, as its process has already been replaced). That process
    /// takes no more jobs in this run, and the jobs left go to the others.
    /// A job no process was left to take is `None`: it failed, unsent.
    ///
    /// `base` starts on the jobs at once. Each other process first builds
    /// `base`'s program (once per pool) and then joins in, so a process that
    /// is still building costs the run no time: the jobs it would have taken
    /// go to a process that is ready, and once no job is left it is stopped
    /// rather than waited for.
    ///
    /// With one process, the jobs run on `base` in order, and the first
    /// answer that leaves no process leaves the rest unsent.
    pub fn run<J, R, D, F>(&self, jobs: &[J], leaves_no_process: D, work: F) -> Vec<Option<R>>
    where
        J: Sync,
        R: Send,
        D: Fn(&R) -> bool + Sync,
        F: Fn(&TypeSidecar, &J) -> R + Sync,
    {
        if self.extra.is_empty() || jobs.len() <= 1 {
            let mut answers: Vec<Option<R>> = Vec::with_capacity(jobs.len());
            for job in jobs {
                let answer = work(self.base, job);
                let gone = leaves_no_process(&answer);
                answers.push(Some(answer));
                if gone {
                    retired(self.base);
                    break;
                }
            }
            answers.resize_with(jobs.len(), || None);
            return unsent_logged(answers);
        }
        let next = AtomicUsize::new(0);
        let answers: Vec<Mutex<Option<R>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
        let workers: Vec<(Option<usize>, &TypeSidecar)> = std::iter::once((None, self.base))
            .chain(self.extra.iter().enumerate().map(|(i, s)| (Some(i), s)))
            .collect();
        std::thread::scope(|scope| {
            for (started, sidecar) in workers {
                let (next, answers, work, leaves_no_process) =
                    (&next, &answers, &work, &leaves_no_process);
                scope.spawn(move || {
                    if started.is_some_and(|index| !self.holds_program(index)) {
                        return;
                    }
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        let Some(job) = jobs.get(index) else {
                            self.stop_the_unready();
                            break;
                        };
                        let answer = work(sidecar, job);
                        let gone = leaves_no_process(&answer);
                        *answers[index].lock().unwrap_or_else(|p| p.into_inner()) = Some(answer);
                        if gone {
                            retired(sidecar);
                            break;
                        }
                    }
                });
            }
        });
        unsent_logged(
            answers
                .into_iter()
                .map(|slot| slot.into_inner().unwrap_or_else(|p| p.into_inner()))
                .collect(),
        )
    }
}

/// The log line for a process [`SidecarPool::run`] hands no more jobs to.
fn retired(sidecar: &TypeSidecar) {
    warn!(
        "A sidecar pool process (pid {}) can no longer answer and takes no more jobs",
        sidecar.pid()
    );
}

/// [`SidecarPool::run`]'s answers, with a log line when some were not sent.
fn unsent_logged<R>(answers: Vec<Option<R>>) -> Vec<Option<R>> {
    let unsent = answers.iter().filter(|answer| answer.is_none()).count();
    if unsent > 0 {
        warn!(
            "No sidecar pool process was left for {unsent} of {} job(s); they were not sent",
            answers.len()
        );
    }
    answers
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

/// A stand-in sidecar for the tests of the pool and of its callers.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    use crate::services::type_sidecar::{RetypeItem, TypeSidecar};

    /// A stand-in sidecar that answers each retype item after a short wait,
    /// so that every process of a pool gets work. An item whose id ends in
    /// an even digit agrees and any other abstains, so an answer depends on
    /// the item alone; every answer names the pid of the process that gave
    /// it as its reason, after a progress frame. A request holding an item
    /// whose id starts with `die` ends the process before it answers, and
    /// one holding an item whose id starts with `fail` is answered with an
    /// error frame.
    ///
    /// It keeps a program's file list as the sidecar does (carrick#2027):
    /// `list_program_files` answers it, `add_program_files` appends the files
    /// it does not hold (refusing a list naming a file with `refuse` in its
    /// path, and taking 3 s over one naming a file with `slow` in it), and a
    /// retype item's file joins it when the item is asked about. Every retype answer carries the list as it stood, joined with
    /// commas, as the message of its one diagnostic.
    pub(crate) fn stand_in(dir: &Path) -> PathBuf {
        let script = dir.join("stand-in-sidecar.cjs");
        std::fs::write(
            &script,
            r#"
const fs = require('fs');
const write = (frame) => fs.writeSync(1, JSON.stringify(frame) + '\n');
const files = [];
const load = (file) => { if (!files.includes(file)) files.push(file); };
require('readline').createInterface({ input: process.stdin, terminal: false }).on('line', (line) => {
  const request = JSON.parse(line);
  const request_id = request.request_id;
  if (request.action === 'shutdown') { write({ request_id, status: 'success' }); process.exit(0); }
  if (request.action === 'init') return write({ request_id, status: 'ready' });
  if (request.action === 'list_program_files') return write({ request_id, status: 'success', files });
  if (request.action === 'add_program_files') {
    if (request.files.some((file) => file.includes('refuse'))) {
      return write({ request_id, status: 'error', errors: ['the stand-in refuses these files'] });
    }
    if (request.files.some((file) => file.includes('slow'))) {
      const until = Date.now() + 3000;
      while (Date.now() < until) {}
    }
    const held = files.length;
    request.files.forEach(load);
    return write({ request_id, status: 'success', added: files.length - held });
  }
  if (request.items.some((item) => item.item_id.startsWith('die'))) process.exit(1);
  if (request.items.some((item) => item.item_id.startsWith('fail'))) {
    return write({ request_id, status: 'error', errors: ['the stand-in fails this request'] });
  }
  write({ request_id, status: 'progress', phase: 'retype', message: `0 of ${request.items.length}` });
  request.items.forEach((item) => load(item.file_path));
  const until = Date.now() + 150;
  while (Date.now() < until) {}
  write({
    request_id,
    status: 'success',
    outcomes: request.items.map((item) => ({
      item_id: item.item_id,
      outcome: Number(item.item_id.slice(-1)) % 2 === 0 ? 'agrees' : 'abstain',
      diagnostics: [{ line: 1, code: 0, message: files.join(',') }],
      reason: String(process.pid),
    })),
  });
});
"#,
        )
        .unwrap();
        script
    }

    /// One retype item for `id`, a call in `file`.
    pub(crate) fn retype_item(file: &str, id: &str) -> RetypeItem {
        RetypeItem {
            item_id: id.to_string(),
            file_path: file.to_string(),
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

    /// A ready stand-in process scoped to `root`, the caller's own sidecar.
    pub(crate) fn base_at(root: &Path) -> TypeSidecar {
        let base = TypeSidecar::spawn(&stand_in(root)).unwrap();
        base.start_init(root, None);
        base.wait_ready(std::time::Duration::from_secs(20)).unwrap();
        base
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{base_at, retype_item};
    use super::*;

    fn item(id: &str) -> crate::services::type_sidecar::RetypeItem {
        retype_item("/repo/src/a.ts", id)
    }

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

    /// A live child's resident size is read from the kernel, as the pool
    /// reads its sidecars': a node process that holds 64 MB it has written
    /// to reads as at least that much. No other process is started for the
    /// read, so it works where `ps` cannot run (a sandbox that refuses setuid
    /// binaries, a container without procps).
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_live_child_s_resident_size_is_read_from_the_kernel() {
        use std::io::BufRead;

        let mut child = std::process::Command::new("node")
            .args([
                "-e",
                "globalThis.held = Buffer.alloc(64 * 1024 * 1024, 1); \
                 console.log('ready'); setTimeout(() => {}, 60000);",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("node starts");
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");

        let bytes = resident_bytes(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let bytes = bytes.expect("the child's size reads");
        assert!(bytes >= 64 * 1024 * 1024, "{bytes} bytes");
        assert_eq!(resident_bytes(u32::MAX), None, "no such process");
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

    /// One job's answer from the stand-in: the pid of the process that ran it.
    fn ask(sidecar: &TypeSidecar, job: &str) -> Result<String, SidecarError> {
        Ok(sidecar
            .retype_check(&[item(job)])?
            .remove(0)
            .reason
            .expect("the stand-in names its pid"))
    }

    /// The predicate a caller hands [`SidecarPool::run`].
    fn gone(answer: &Result<String, SidecarError>) -> bool {
        matches!(answer, Err(e) if e.leaves_no_process())
    }

    /// Jobs are answered by every process of the pool, and the answers come
    /// back in the order of the jobs whichever process ran them. The extra
    /// processes are scoped to the same root as the caller's.
    #[test]
    fn a_pool_answers_every_job_in_order_across_its_processes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = base_at(&root);

        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);
        let jobs: Vec<String> = (0..12).map(|n| format!("job-{n}")).collect();
        let answers = pool.run(
            &jobs,
            |_: &(String, String)| false,
            |sidecar, job| {
                assert!(sidecar.is_scoped_to(&root, None));
                let outcome = sidecar
                    .retype_check(&[item(job)])
                    .expect("the stand-in answers")
                    .remove(0);
                (outcome.item_id, outcome.reason.unwrap())
            },
        );
        let answers: Vec<(String, String)> = answers
            .into_iter()
            .map(|answer| answer.expect("every job is sent"))
            .collect();
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
        let answers = single.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        let own = base.pid().to_string();
        assert!(
            answers
                .iter()
                .all(|answer| matches!(answer, Some(Ok(pid)) if *pid == own)),
            "{answers:?}"
        );
    }

    /// carrick#2027: every process a pool starts holds the caller's program
    /// before its first job: the files the caller's program was built from,
    /// in their order. The caller was asked about two files in an order no
    /// sort would give, and every job, whichever process took it, is judged
    /// in a program holding those two in that order. A process that cannot
    /// take the caller's files takes no job; the caller answers them all.
    #[test]
    fn every_process_of_a_pool_holds_the_callers_program_before_its_first_job() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = base_at(&root);
        base.retype_check(&[
            retype_item("/repo/src/late.ts", "x-0"),
            retype_item("/repo/src/early.ts", "x-2"),
        ])
        .unwrap();
        assert_eq!(
            base.program_files().unwrap(),
            [
                std::path::PathBuf::from("/repo/src/late.ts"),
                std::path::PathBuf::from("/repo/src/early.ts")
            ]
        );

        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);
        let jobs: Vec<String> = (0..12).map(|n| format!("job-{n}")).collect();
        let answers = pool.run(
            &jobs,
            |_: &(String, String)| false,
            |sidecar, job| {
                let mut outcome = sidecar
                    .retype_check(&[retype_item("/repo/src/late.ts", job)])
                    .expect("the stand-in answers")
                    .remove(0);
                (
                    outcome.reason.unwrap(),
                    outcome.diagnostics.remove(0).message,
                )
            },
        );
        let answers: Vec<(String, String)> = answers.into_iter().map(Option::unwrap).collect();
        let pids: std::collections::BTreeSet<&String> =
            answers.iter().map(|(pid, _)| pid).collect();
        assert_eq!(
            pids.len(),
            3,
            "every process answered some job: {answers:?}"
        );
        assert!(
            answers
                .iter()
                .all(|(_, program)| program == "/repo/src/late.ts,/repo/src/early.ts"),
            "{answers:?}"
        );

        let refused = base_at(&root);
        refused
            .retype_check(&[retype_item("/repo/src/refuse.ts", "x-0")])
            .unwrap();
        let pool = SidecarPool::scoped(&refused, &root, None, 3).unwrap();
        let answers = pool.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        let own = refused.pid().to_string();
        assert!(
            answers
                .iter()
                .all(|answer| matches!(answer, Some(Ok(pid)) if *pid == own)),
            "a process without the program takes no job: {answers:?}"
        );
    }

    /// A started process still building the program costs the run no time:
    /// the caller starts on the jobs at once, answers all four before the
    /// others' 3 s builds end, and the run returns without waiting for them.
    /// A process stopped that way takes no job in a later run either.
    #[test]
    fn the_caller_does_not_wait_for_a_process_still_building_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = base_at(&root);
        base.retype_check(&[retype_item("/repo/src/slow.ts", "x-0")])
            .unwrap();
        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);
        let jobs: Vec<String> = (0..4).map(|n| format!("job-{n}")).collect();
        let started = std::time::Instant::now();
        let answers = pool.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        let took = started.elapsed();
        let own = base.pid().to_string();
        assert!(
            answers
                .iter()
                .all(|answer| matches!(answer, Some(Ok(pid)) if *pid == own)),
            "{answers:?}"
        );
        assert!(
            took < std::time::Duration::from_millis(2500),
            "the run waited for the builds: {took:?}"
        );
        let again = pool.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        assert!(
            again
                .iter()
                .all(|answer| matches!(answer, Some(Ok(pid)) if *pid == own)),
            "{again:?}"
        );
    }

    /// A process that dies takes no job after the one it died on: the jobs
    /// left go to the processes still answering, and every one is answered.
    #[test]
    fn a_dead_process_takes_no_more_jobs_and_the_others_answer_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = base_at(&root);
        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);

        let mut jobs: Vec<String> = (0..12).map(|n| format!("job-{n}")).collect();
        jobs[1] = "die".to_string();
        let answers = pool.run(&jobs, gone, |sidecar, job| ask(sidecar, job));

        assert!(
            matches!(&answers[1], Some(Err(e)) if e.leaves_no_process()),
            "the job its process died on is that job's failure: {:?}",
            answers[1]
        );
        for (index, answer) in answers.iter().enumerate().filter(|(index, _)| *index != 1) {
            assert!(
                matches!(answer, Some(Ok(_))),
                "job {index} is answered by a process still running: {answers:?}"
            );
        }
    }

    /// With every process dead, the jobs not yet taken come back unsent.
    /// Each process takes one job and dies on it, so the first three are
    /// failures and the rest are never sent. One process alone stops the
    /// same way.
    #[test]
    fn with_every_process_dead_the_jobs_left_come_back_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base = base_at(&root);
        let pool = SidecarPool::scoped(&base, &root, None, 3).unwrap();
        assert_eq!(pool.processes(), 3);

        let jobs: Vec<String> = (0..12).map(|n| format!("die-{n}")).collect();
        let answers = pool.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        assert!(
            answers[..3]
                .iter()
                .all(|answer| matches!(answer, Some(Err(e)) if e.leaves_no_process())),
            "each process died on the one job it took: {answers:?}"
        );
        assert!(
            answers[3..].iter().all(Option::is_none),
            "no process was left for the rest: {answers:?}"
        );

        let alone = base_at(&root);
        let single = SidecarPool::scoped(&alone, &root, None, 1).unwrap();
        let jobs = ["die".to_string(), "job-1".to_string(), "job-2".to_string()];
        let answers = single.run(&jobs, gone, |sidecar, job| ask(sidecar, job));
        assert!(
            matches!(&answers[0], Some(Err(e)) if e.leaves_no_process()),
            "{answers:?}"
        );
        assert!(answers[1..].iter().all(Option::is_none), "{answers:?}");
    }
}
