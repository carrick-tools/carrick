//! When a re-check answers through the resident type daemon (carrick#2884).
//!
//! The daemon itself, and how to talk to it, is
//! [`crate::services::typed_daemon`]. This module decides, for one
//! `check --changed`, `check --recheck` or `check --each` re-scan:
//!
//! - **Use it** when a daemon of this build serves the workspace: each
//!   service it holds a built program for answers through it; every other
//!   service answers through a process of its own, as with no daemon, so the
//!   services nobody warmed are never queued one behind another in the
//!   daemon's single queue.
//! - **Start it** when there is none, or one of another build (killed: a
//!   stale build never answers). The run that found it missing answers cold,
//!   exactly as before, and the daemon is started and warmed in the
//!   background afterwards ([`Session::close`]): `carrick daemon start
//!   --warm` re-scans the changed repos through it, so the next run's
//!   requests are ones it has already answered.
//! - **Leave it** when it is busy answering someone else: this run answers
//!   cold and nothing is started.
//!
//! Output and exit codes do not depend on which of these happened: the
//! daemon answers the same requests with the same answers (carrick#2881's
//! warm == cold tests), and any daemon failure falls back to a process of the
//! sidecar's own for that request ([`crate::services::TypeSidecar`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::services::typed_daemon::{self, Probe, Route, Use};

/// How long a re-check waits for the daemon to answer `hello` and `status`
/// before treating it as busy. Each is answered at once by an idle daemon.
const PROBE_WAIT: Duration = Duration::from_secs(1);
/// How long `carrick daemon start` waits for a daemon it started to answer.
const START_WAIT: Duration = Duration::from_secs(15);
/// The longest one warm-up re-scan may run in the background.
const WARM_BUDGET: Duration = Duration::from_secs(30 * 60);
/// The warm-up's claim, inside the index directory: one at a time per
/// workspace.
const WARM_CLAIM: &str = "typed-warm.pid";

/// How a re-scan uses the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// A re-check: use the daemon for what it holds, start or warm it after.
    Probe,
    /// The warm-up: every service through the daemon, nothing started after.
    All,
}

/// One re-check's use of the daemon, from before its re-scans to after its
/// answer.
pub(super) struct Session {
    root: PathBuf,
    mode: Mode,
    daemon: Option<Use>,
    /// Start and warm the daemon once the answer is read.
    warm_after: bool,
    /// Where the re-scans list the services they answered without it.
    misses: PathBuf,
    /// Every service the daemon's status lists, in any state: a miss on one
    /// warms nothing ([`warms_after`]).
    listed: BTreeSet<PathBuf>,
}

impl Session {
    pub(super) fn open(root: &Path, generation: &Path, mode: Mode) -> Self {
        let misses = generation.join("typed-misses");
        let mut session = Self {
            root: root.to_path_buf(),
            mode,
            daemon: None,
            warm_after: false,
            misses: misses.clone(),
            listed: BTreeSet::new(),
        };
        if !typed_daemon::enabled() {
            return session;
        }
        let Some(build_id) = sidecar_build_id() else {
            return session;
        };
        // A build that crashed twice within the hour is neither used nor
        // started again for a day (the lifecycle policy's L2).
        if let Some(back_off) = typed_daemon::backed_off(root, &build_id) {
            tracing::debug!(
                "The type daemon is off after {} crashes; this re-check answers cold",
                back_off.crashes
            );
            return session;
        }
        if mode == Mode::All {
            // The warm-up fills the daemon, once it has answered `hello` on
            // this build: a record names a socket, not who listens on it.
            session.daemon = warm_up_use(root, &build_id);
            return session;
        }
        match typed_daemon::probe(root, &build_id, PROBE_WAIT) {
            // A warm-up holds the daemon's one queue with minute-long
            // requests: this check answers cold and starts no second one.
            Probe::Serving(status) if status.warming => {
                tracing::debug!(
                    "The type daemon (pid {}) is warming; this re-check answers cold",
                    status.pid
                );
            }
            Probe::Serving(status) => {
                session.listed = status.listed();
                session.daemon = Some(Use::new(
                    status.socket.clone(),
                    Route::Held(status.built()),
                    Some(misses),
                ));
            }
            Probe::Busy { pid } => {
                tracing::debug!("The type daemon (pid {pid}) is busy; this re-check answers cold");
            }
            Probe::Mismatch { pid } => {
                tracing::debug!("The type daemon (pid {pid}) runs another build; replacing it");
                typed_daemon::retire(root, pid);
                session.warm_after = true;
            }
            Probe::Absent => session.warm_after = true,
        }
        session
    }

    /// Hand the daemon, when this run uses one, to the scan `command` starts.
    pub(super) fn apply(&self, command: &mut Command) {
        // The warm-up's scans run one service at a time, so they and the
        // daemon's queue use one core between them (L1).
        if self.mode == Mode::All {
            command.env(crate::engine::service_workers::SERVICE_CONCURRENCY_ENV, "1");
        }
        match &self.daemon {
            Some(daemon) => daemon.apply(command),
            // An inherited handle is not this run's to use.
            None => {
                command
                    .env_remove(typed_daemon::SOCKET_ENV)
                    .env_remove(typed_daemon::ROUTE_ENV)
                    .env_remove(typed_daemon::MISSES_ENV);
            }
        }
    }

    /// Start the warm-up in the background when this run found no daemon (or
    /// one of another build), or answered without it a service the daemon has
    /// never seen (L1).
    pub(super) fn close(self) {
        if self.mode != Mode::Probe {
            return;
        }
        let misses = std::fs::read_to_string(&self.misses).unwrap_or_default();
        if warms_after(self.warm_after, &misses, &self.listed) {
            start_warm_up(&self.root);
        }
    }
}

/// Whether a re-check starts the warm-up: when it found no daemon to use
/// (`warm_after`), or one of its misses is a service the daemon's status does
/// not list at all. A service it lists, in any state (built, released after
/// an eviction, cold only), starts nothing: the daemon has had it, and warming
/// it again after every check would evict the next service a check needs, in a
/// loop.
fn warms_after(warm_after: bool, misses: &str, listed: &BTreeSet<PathBuf>) -> bool {
    warm_after
        || misses
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .any(|line| !listed.contains(Path::new(line)))
}

/// The warm-up's use of the daemon serving `root`: every service through it,
/// when it answers `hello` with `build_id`. Otherwise none, and the warm-up's
/// re-scan answers cold.
fn warm_up_use(root: &Path, build_id: &str) -> Option<Use> {
    match typed_daemon::probe(root, build_id, PROBE_WAIT) {
        Probe::Serving(status) => Some(Use::new(status.socket, Route::All, None)),
        Probe::Busy { .. } | Probe::Mismatch { .. } | Probe::Absent => None,
    }
}

/// The build id of the sidecar a scan from this binary would spawn.
fn sidecar_build_id() -> Option<String> {
    let entry = crate::services::type_sidecar::discover_sidecar_path()?;
    typed_daemon::build_id(&entry).ok()
}

/// `carrick daemon start [--warm]`, detached: in a process group of its own
/// with nothing to write to, so it outlives the command that started it.
fn start_warm_up(root: &Path) {
    if warm_up_running(root) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command
        .args(["daemon", "start", "--warm", "--workspace"])
        .arg(root)
        // Nobody is waiting on it, so it holds nothing awake (carrick#1889).
        .env(crate::keep_awake::OFF_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Err(e) = command.spawn() {
        tracing::debug!("The type daemon's warm-up did not start: {e}");
    }
}

/// Whether a warm-up holds the claim for `root` and is still running.
fn warm_up_running(root: &Path) -> bool {
    std::fs::read_to_string(root.join(super::workspace::INDEX_DIR).join(WARM_CLAIM))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .is_some_and(typed_daemon::pid_alive)
}

/// The warm-up's claim, released when dropped.
struct WarmClaim(PathBuf);

impl WarmClaim {
    /// Take the claim for `root`, or `None` when a live warm-up holds it. A
    /// claim whose process has exited is taken over.
    fn take(root: &Path) -> Option<Self> {
        let path = root.join(super::workspace::INDEX_DIR).join(WARM_CLAIM);
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", std::process::id());
                    return Some(Self(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if warm_up_running(root) {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for WarmClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `carrick daemon start`: a daemon of this build serving the workspace,
/// started when there is none and replacing one of another build. With
/// `warm`, every repo with changes since its main branch is then re-scanned
/// through it, so a re-check of those changes finds their programs built and
/// their answers kept. Returns the line to print.
pub(super) fn start(root: Option<&Path>, warm: bool) -> Result<String, String> {
    if !typed_daemon::enabled() {
        return Err(format!(
            "the type daemon is off here (set {}=on to use it; it needs Unix sockets)",
            typed_daemon::ON_ENV
        ));
    }
    let root = super::workspace::locate(root, None)
        .ok_or("no Carrick index above this directory. Run `carrick init` in the folder holding your repos.")?;
    let entry = crate::services::type_sidecar::discover_sidecar_path()
        .ok_or("the type sidecar was not found, so there is nothing to start")?;
    let build_id = typed_daemon::build_id(&entry)
        .map_err(|e| format!("could not read the sidecar's build: {e}"))?;
    if let Some(back_off) = typed_daemon::backed_off(&root, &build_id) {
        return Err(format!(
            "the type daemon is off for this build after {} crashes; its log is {}",
            back_off.crashes,
            root.join(super::workspace::INDEX_DIR)
                .join("typed.log")
                .display()
        ));
    }
    let started = Instant::now();
    match typed_daemon::probe(&root, &build_id, PROBE_WAIT) {
        Probe::Serving(_) | Probe::Busy { .. } => {}
        Probe::Mismatch { pid } => {
            typed_daemon::retire(&root, pid);
            typed_daemon::start_detached(&root, &entry)
                .map_err(|e| format!("could not start the type daemon: {e}"))?;
        }
        Probe::Absent => typed_daemon::start_detached(&root, &entry)
            .map_err(|e| format!("could not start the type daemon: {e}"))?,
    }
    let status = typed_daemon::wait_serving(&root, &build_id, START_WAIT).ok_or_else(|| {
        format!(
            "the type daemon did not answer within {}s; its log is {}",
            START_WAIT.as_secs(),
            root.join(super::workspace::INDEX_DIR)
                .join("typed.log")
                .display()
        )
    })?;
    let serving = format!(
        "Type daemon: pid {}, serving {}.",
        status.pid,
        root.display()
    );
    if !warm {
        return Ok(serving);
    }
    let Some(_claim) = WarmClaim::take(&root) else {
        return Ok(format!("{serving} A warm-up is already running."));
    };
    let index = super::query::read_index(&root).map_err(|failure| failure.message().to_string())?;
    let workspace = super::workspace::Workspace::load(&root)?;
    let (_, changed) = super::changed::changed_files(&workspace, &index);
    let repos: Vec<&Path> = changed.values().map(|(path, _)| path.as_path()).collect();
    if repos.is_empty() {
        return Ok(format!("{serving} No repo has changes to warm it for."));
    }
    // The daemon was started at this process's priority, so it answers the
    // checks the user waits on at full speed; the warm-up's own scans run at
    // background priority from here (L1).
    lower_own_priority();
    let generation =
        std::env::temp_dir().join(format!("carrick-typed-warm-{}", uuid::Uuid::new_v4()));
    let warmed = super::recheck::rejoin(
        &workspace,
        &repos,
        &generation,
        Instant::now() + WARM_BUDGET,
        Mode::All,
        None,
    );
    let _ = std::fs::remove_dir_all(&generation);
    warmed?;
    Ok(format!(
        "{serving} Warmed for the changes in {} repo(s) in {:.1}s.",
        repos.len(),
        started.elapsed().as_secs_f64()
    ))
}

/// Run this process at background priority from now on: macOS's background
/// band (lower CPU and I/O priority), Linux's lowest nice. Children started
/// after inherit it.
fn lower_own_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: setpriority on this process (who 0) with the darwin background
    // band; it changes nothing but scheduling.
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: setpriority on this process (who 0) to nice 19.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}

/// `carrick daemon stop`.
pub(super) fn stop(root: Option<&Path>) -> Result<String, String> {
    let root = super::workspace::locate(root, None)
        .ok_or("no Carrick index above this directory, so no type daemon serves it")?;
    // The warm-up first: left running, it would go on re-scanning through a
    // daemon that is gone.
    let warm_up = stop_warm_up(&root);
    let daemon = typed_daemon::stop(&root);
    Ok(match warm_up {
        Some(line) => format!("{line}\n{daemon}"),
        None => daemon,
    })
}

/// How long a warm-up's start may precede its claim: it starts the daemon and
/// waits for it to serve ([`START_WAIT`]) before it takes the claim.
const WARM_CLAIM_SLACK: Duration = Duration::from_secs(60);
/// How long a warm-up has to exit after SIGTERM before SIGKILL: it ends its
/// re-scans on the signal first (carrick#2923).
const WARM_STOP_GRACE: Duration = Duration::from_secs(5);

/// End the warm-up holding the claim for `root`, when there is one, and say
/// what happened. It is signalled only when proven to be this workspace's
/// warm-up: its command line is `daemon start --warm --workspace <root>` and
/// it started at most [`WARM_CLAIM_SLACK`] before its claim was written. A
/// claim naming anything else is stale: removed, its process left alone.
fn stop_warm_up(root: &Path) -> Option<String> {
    let claim = root.join(super::workspace::INDEX_DIR).join(WARM_CLAIM);
    let pid = std::fs::read_to_string(&claim)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    if !typed_daemon::pid_alive(pid) {
        let _ = std::fs::remove_file(&claim);
        return None;
    }
    let claimed_at = std::fs::metadata(&claim)
        .and_then(|meta| meta.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from);
    if warm_up_proven(pid, root, claimed_at) {
        let ended = typed_daemon::end_proven(pid, WARM_STOP_GRACE);
        let _ = std::fs::remove_file(&claim);
        return Some(format!(
            "Stopped the type daemon's warm-up (pid {pid}) with {ended}."
        ));
    }
    let _ = std::fs::remove_file(&claim);
    Some(format!(
        "Removed a stale warm-up claim: pid {pid} is not this workspace's warm-up and was left running."
    ))
}

/// Whether process `pid` is the warm-up of `root` that wrote a claim at
/// `claimed_at`.
fn warm_up_proven(
    pid: u32,
    root: &Path,
    claimed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    let (Some((started, command)), Some(claimed_at)) =
        (typed_daemon::process_start(pid), claimed_at)
    else {
        return false;
    };
    let names = |path: &Path| {
        command.contains(&format!(
            "daemon start --warm --workspace {}",
            path.display()
        ))
    };
    let fits = started <= claimed_at + chrono::Duration::seconds(2)
        && claimed_at - started <= chrono::Duration::from_std(WARM_CLAIM_SLACK).unwrap_or_default();
    fits && (names(root) || root.canonicalize().is_ok_and(|real| names(&real)))
}

/// What `carrick status` says about the daemon: its lines, and its status
/// object for `--json`.
pub(super) fn status_report(root: &Path) -> (Vec<String>, Option<serde_json::Value>) {
    typed_daemon::status_report(root, sidecar_build_id().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record naming a live process and a socket nobody answers on is not a
    /// daemon to fill: the warm-up answers cold rather than sending its
    /// re-scan to whatever the record names.
    #[test]
    fn the_warm_up_uses_only_a_daemon_that_answered_hello() {
        let dir = tempfile::tempdir().unwrap();
        let carrick = dir.path().join(".carrick");
        std::fs::create_dir_all(&carrick).unwrap();
        let socket = carrick.join("typed.sock");
        std::fs::write(&socket, "not a socket").unwrap();
        std::fs::write(
            carrick.join("typed.json"),
            serde_json::json!({ "pid": std::process::id(), "socket": socket }).to_string(),
        )
        .unwrap();
        assert!(warm_up_use(dir.path(), "any build").is_none());
    }

    /// A miss warms only a service the daemon has never listed: one it
    /// released after an eviction, or holds cold only, starts nothing, so a
    /// check after an eviction does not start the warm-up that evicts again.
    #[test]
    fn only_a_service_the_daemon_never_saw_starts_a_warm_up() {
        let listed: BTreeSet<PathBuf> = [PathBuf::from("/w/a"), PathBuf::from("/w/b")].into();
        assert!(!warms_after(false, "/w/a\n/w/b\n", &listed));
        assert!(!warms_after(false, "", &listed));
        assert!(warms_after(false, "/w/a\n/w/c\n", &listed));
        assert!(warms_after(true, "", &listed));
    }
}
