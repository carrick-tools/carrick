//! What this machine can hold: the memory it can hand out now, a process's
//! resident size, and how many of one kind of work fit beside each other.
//!
//! Two callers size work by the same rule with different limits: a sidecar
//! pool (several processes over one service's program, carrick#1996) and the
//! services of a scan analysed side by side (carrick#1066). Each states its
//! limits as a [`Sizing`]; the arithmetic and the log line are one.

/// Memory left free for everything else on the machine (the scanner, the
/// editor and browser of whoever runs the scan), never handed out.
pub const HEADROOM_MB: u64 = 2048;

/// The limits one kind of work is sized under.
#[derive(Debug, Clone, Copy)]
pub struct Sizing {
    /// What is counted, for the log line: `"process(es)"`, `"service(s)"`.
    pub noun: &'static str,
    /// The env var that sets the count instead, named in the log line.
    pub knob: &'static str,
    /// The most there are, whatever the machine holds.
    pub cap: usize,
    /// The fewest the cores allow: one less than the cores, and never fewer
    /// than this.
    pub cores_floor: usize,
    /// How many are already running before any is started, counted in the
    /// total but not against the free memory: a pool's caller's own process.
    pub running: usize,
    /// Whether a count set by [`Self::knob`] is still capped by memory.
    pub knob_capped_by_memory: bool,
}

/// How many, and why, for the scan's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Size {
    /// Never below one.
    pub count: usize,
    pub why: String,
}

/// How many of the work `sizing` describes fit at once, each taking about
/// `per_mb`: as many as the free memory holds after [`HEADROOM_MB`], no more
/// than one per spare core (at least [`Sizing::cores_floor`]) and
/// [`Sizing::cap`] in all. `forced` (the knob's value) sets the count instead.
pub fn size_for(
    sizing: &Sizing,
    available_mb: Option<u64>,
    per_mb: u64,
    cores: usize,
    forced: Option<usize>,
) -> Size {
    let Sizing {
        noun,
        knob,
        cap,
        cores_floor,
        running,
        knob_capped_by_memory,
    } = *sizing;
    let per = per_mb.max(1);
    let Some(available) = available_mb else {
        return match forced {
            Some(count) if !knob_capped_by_memory => Size {
                count,
                why: format!("{count} {noun}: asked for by {knob}"),
            },
            _ => Size {
                count: 1,
                why: format!("one {}: the free memory could not be read", singular(noun)),
            },
        };
    };
    let by_memory = running + (available.saturating_sub(HEADROOM_MB) / per) as usize;
    let by_cores = cores.saturating_sub(1).max(cores_floor);
    let wanted = forced.unwrap_or_else(|| by_cores.min(cap));
    let count = if forced.is_some() && !knob_capped_by_memory {
        wanted
    } else {
        wanted.min(by_memory)
    }
    .max(1);
    let why = format!(
        "{count} {noun}: {wanted} {} and {available} MB free holds {by_memory} at {per} MB each after {HEADROOM_MB} MB kept back",
        if forced.is_some() {
            format!("asked for by {knob}")
        } else {
            format!("allowed by {cores} core(s)")
        },
    );
    Size { count, why }
}

fn singular(noun: &str) -> &str {
    noun.strip_suffix("(es)")
        .or_else(|| noun.strip_suffix("(s)"))
        .unwrap_or(noun)
}

/// The cores this process may use, at least one.
pub fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// The memory this run's task may use, in MB, set by a runner that knows its
/// box (carrick#3084). The scan then spends at most this, less what it and
/// its sidecars already hold, whatever the host's own reading says.
/// Documented for runners in `docs/reference/runner.md`.
pub const MEMORY_LIMIT_ENV: &str = "CARRICK_MEMORY_LIMIT_MB";

/// Where the figure [`available_memory`] settled on came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySource {
    /// [`MEMORY_LIMIT_ENV`], less what this process and its sidecars hold.
    Env,
    /// The cgroup's limit, less what the cgroup uses.
    Cgroup,
    /// The machine's own reading.
    Machine,
}

impl MemorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            MemorySource::Env => "env",
            MemorySource::Cgroup => "cgroup",
            MemorySource::Machine => "machine",
        }
    }
}

/// Memory this run can hand out now, and where the figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Available {
    pub mb: u64,
    pub source: MemorySource,
}

/// Memory this run can hand out now, in MB: [`available_memory`]'s figure.
/// `None` when it cannot be read.
pub fn available_memory_mb() -> Option<u64> {
    available_memory().map(|available| available.mb)
}

/// Memory this run can hand out now (carrick#3084): the machine's reading,
/// lowered to the first of these that is known:
/// - [`MEMORY_LIMIT_ENV`] less what this process and every live sidecar hold;
/// - the cgroup's limit less what the cgroup uses (v2, then v1).
///
/// A container that sets no memory of its own reads the host's, which is not
/// the run's to spend. With neither known, the reading is the machine's.
pub fn available_memory() -> Option<Available> {
    let env_limit = env_limit_mb();
    settle(
        machine_available_mb(),
        env_limit.map(|limit| (limit, held_mb())),
        if env_limit.is_some() {
            None
        } else {
            cgroup_memory_left_mb()
        },
    )
}

/// [`available_memory`] over its readings: the machine's, the env limit with
/// what is held against it, and what the cgroup has left. The env's figure is
/// read first, the cgroup's only without it; either lowers the machine's when
/// it is lower, and stands alone when the machine cannot be read.
fn settle(
    machine: Option<u64>,
    env_limit_and_held: Option<(u64, u64)>,
    cgroup_left: Option<u64>,
) -> Option<Available> {
    let limited = env_limit_and_held
        .map(|(limit, held)| (limit.saturating_sub(held), MemorySource::Env))
        .or_else(|| cgroup_left.map(|left| (left, MemorySource::Cgroup)));
    match (machine, limited) {
        (Some(machine), Some((left, source))) if left < machine => {
            Some(Available { mb: left, source })
        }
        (Some(machine), _) => Some(Available {
            mb: machine,
            source: MemorySource::Machine,
        }),
        (None, Some((left, source))) => Some(Available { mb: left, source }),
        (None, None) => None,
    }
}

fn env_limit_mb() -> Option<u64> {
    std::env::var(MEMORY_LIMIT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|mb| *mb > 0)
}

/// What this process and every sidecar it still owns hold now, in MB.
fn held_mb() -> u64 {
    let own = resident_bytes(std::process::id()).unwrap_or(0);
    let sidecars: u64 = crate::services::type_sidecar::live_sidecar_pids()
        .into_iter()
        .filter_map(resident_bytes)
        .sum();
    (own + sidecars) / (1024 * 1024)
}

/// The machine's own reading, in MB: what is free plus what the system
/// would reclaim first.
fn machine_available_mb() -> Option<u64> {
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
        return Some(kb / 1024);
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("vm_stat").output().ok()?;
        return vm_stat_available_mb(&String::from_utf8_lossy(&out.stdout));
    }
    #[allow(unreachable_code)]
    None
}

/// cgroup v2's files, then v1's: the limit, and what the cgroup uses now.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CGROUP_FILES: [(&str, &str); 2] = [
    ("/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory.current"),
    (
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
        "/sys/fs/cgroup/memory/memory.usage_in_bytes",
    ),
];

/// The cgroup memory limit in MB, when this process runs under one.
///
/// `/proc/meminfo` reports the HOST's memory, so a scanner inside a
/// memory-limited container would otherwise derive a cap from memory it
/// cannot touch and be OS-killed at the limit. cgroup v2 first, then v1.
#[cfg(target_os = "linux")]
pub fn cgroup_memory_limit_mb() -> Option<u64> {
    CGROUP_FILES
        .iter()
        .find_map(|(limit, _)| cgroup_limit_mb(&std::fs::read_to_string(limit).ok()?))
}

#[cfg(not(target_os = "linux"))]
pub fn cgroup_memory_limit_mb() -> Option<u64> {
    None
}

/// What the cgroup has left under its limit, in MB: v2's `memory.max` less
/// `memory.current`, then v1's `limit_in_bytes` less `usage_in_bytes`.
#[cfg(target_os = "linux")]
fn cgroup_memory_left_mb() -> Option<u64> {
    CGROUP_FILES.iter().find_map(|(limit, usage)| {
        cgroup_left_mb(
            &std::fs::read_to_string(limit).ok()?,
            &std::fs::read_to_string(usage).ok()?,
        )
    })
}

#[cfg(not(target_os = "linux"))]
fn cgroup_memory_left_mb() -> Option<u64> {
    None
}

/// A cgroup limit file's text in MB. `max` (v2) and v1's near-`u64::MAX`
/// sentinel both read as no limit: anything past a terabyte is that
/// sentinel, not a real container limit.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit_mb(text: &str) -> Option<u64> {
    let mb = text.trim().parse::<u64>().ok()? / (1024 * 1024);
    (mb > 0 && mb <= 1024 * 1024).then_some(mb)
}

/// What is left under a cgroup's limit, in MB, from its limit and usage
/// files' texts. `None` when there is no limit.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_left_mb(limit_text: &str, usage_text: &str) -> Option<u64> {
    let limit = cgroup_limit_mb(limit_text)?;
    let used = usage_text.trim().parse::<u64>().ok()? / (1024 * 1024);
    Some(limit.saturating_sub(used))
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

/// The resident size of process `pid` in bytes, from `proc_pidinfo`.
///
/// Asked of the kernel, not of `ps`: `ps` is setuid on macOS, so a sandbox
/// that refuses setuid binaries refuses it, and a slim container may not
/// have it.
#[cfg(target_os = "macos")]
pub fn resident_bytes(pid: u32) -> Option<u64> {
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
pub fn resident_bytes(pid: u32) -> Option<u64> {
    proc_status_kb(pid, "VmRSS:").map(|kb| kb * 1024)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// The most memory process `pid` has held so far, in bytes: its lifetime
/// peak physical footprint, from `proc_pid_rusage`. Read while it runs, so
/// the process is not reaped for it.
#[cfg(target_os = "macos")]
pub fn peak_resident_bytes(pid: u32) -> Option<u64> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: a zeroed `rusage_info_v4` is a valid value of that plain C struct.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: with `RUSAGE_INFO_V4` the kernel writes one `rusage_info_v4`.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast(),
        )
    };
    (status == 0).then_some(info.ri_lifetime_max_phys_footprint)
}

/// The most memory process `pid` has held so far, in bytes: its resident
/// high-water mark, from `/proc/<pid>/status`.
#[cfg(target_os = "linux")]
pub fn peak_resident_bytes(pid: u32) -> Option<u64> {
    proc_status_kb(pid, "VmHWM:").map(|kb| kb * 1024)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn peak_resident_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn proc_status_kb(pid: u32, field: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live child's resident size and its peak are read from the kernel: a
    /// node process that holds 64 MB it has written to reads as at least that
    /// much, now and at its peak. No other process is started for the read,
    /// so it works where `ps` cannot run.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_live_child_s_resident_size_and_peak_are_read_from_the_kernel() {
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
        let peak = peak_resident_bytes(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let bytes = bytes.expect("the child's size reads");
        assert!(bytes >= 64 * 1024 * 1024, "{bytes} bytes");
        let peak = peak.expect("the child's peak reads");
        assert!(peak >= 64 * 1024 * 1024, "{peak} bytes at its peak");
        assert_eq!(resident_bytes(u32::MAX), None, "no such process");
        assert_eq!(peak_resident_bytes(u32::MAX), None, "no such process");
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

    /// carrick#3084: v2's and v1's files read the same, and a cgroup with no
    /// limit reads as none.
    #[test]
    fn cgroup_v1_and_v2_texts_read_the_same() {
        const GB: u64 = 1024 * 1024 * 1024;
        let limit = format!("{}\n", 16 * GB);
        let used = format!("{}\n", 5 * GB);
        // v2 and v1 carry the same numbers in the same shape.
        assert_eq!(cgroup_left_mb(&limit, &used), Some(11 * 1024));
        assert_eq!(cgroup_limit_mb(&limit), Some(16 * 1024));
        // v2's no-limit word, and v1's sentinel.
        assert_eq!(cgroup_left_mb("max\n", &used), None);
        assert_eq!(cgroup_left_mb("9223372036854771712\n", &used), None);
        assert_eq!(cgroup_limit_mb("max"), None);
        // Over the limit: nothing left, never a wrap.
        assert_eq!(cgroup_left_mb(&used, &limit), Some(0));
        assert_eq!(cgroup_left_mb(&limit, "garbage"), None);
    }

    /// carrick#3084: with the env set, the reading is the limit less what
    /// this process and its sidecars hold, lowering the machine's; without
    /// it, the cgroup's; with neither, the machine's, as before.
    #[test]
    fn the_machine_reading_is_lowered_to_a_known_limit() {
        let available = |mb, source| Some(Available { mb, source });
        // The host reads 29 GB; the task may use 16 GB, and 1.7 GB is held.
        assert_eq!(
            settle(Some(29_720), Some((16_384, 1_700)), None),
            available(14_684, MemorySource::Env)
        );
        // The env wins over the cgroup, which is not read beside it.
        assert_eq!(
            settle(Some(29_720), Some((16_384, 1_700)), Some(4_096)),
            available(14_684, MemorySource::Env)
        );
        // A machine reading below the limit stands.
        assert_eq!(
            settle(Some(8_000), Some((16_384, 1_700)), None),
            available(8_000, MemorySource::Machine)
        );
        // More held than the limit: nothing left, never a wrap.
        assert_eq!(
            settle(Some(8_000), Some((1_024, 1_700)), None),
            available(0, MemorySource::Env)
        );
        assert_eq!(
            settle(Some(29_720), None, Some(11_264)),
            available(11_264, MemorySource::Cgroup)
        );
        assert_eq!(
            settle(None, None, Some(4_096)),
            available(4_096, MemorySource::Cgroup)
        );
        assert_eq!(
            settle(Some(12_000), None, None),
            available(12_000, MemorySource::Machine)
        );
        assert_eq!(settle(None, None, None), None);
    }

    /// What is held against the env's limit is read from the kernel: this
    /// test process alone holds some.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn what_this_process_holds_is_read() {
        assert!(held_mb() > 0);
    }
}
