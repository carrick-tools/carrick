//! The signals that end a scan, and how a pass in the middle of one hears
//! about them (carrick#1125, carrick#1235, carrick#1387).
//!
//! `main` races the analysis future against a signal, and that race is polled
//! by one task: a stretch of the scan that does not yield is a stretch in
//! which no signal is acted on, and most of a scan is such a stretch — the
//! parse and analysis passes are CPU-bound Rust, and the wait on the type
//! sidecar blocks rather than awaiting. A SIGTERM at one second of a
//! nine-second scan ran to completion, exit 0, with nothing said to the cloud
//! (carrick#1387).
//!
//! So the signal is heard in a task of its own, which is polled whatever the
//! analysis is doing, and it does two things with what it hears: it records it
//! where any pass can read it with [`check`], and it sends it to whoever is
//! racing. A pass that notices returns [`Interrupted`] as its error, which
//! ends the run through the path every other failure takes — the engine's own
//! `Err` branch marks the scan failed and ships the log — and `main` then
//! leaves with the signal's exit code rather than with 1.
//!
//! Ending it that way rather than from the watcher is deliberate: a
//! `process::exit` in a task would skip the drop of the analysis future, and
//! the sidecar process it owns would be left running (carrick#1124).
//!
//! The watcher is a tokio task, so it is polled on a worker thread other than
//! the one the analysis is on. A runtime with a single worker thread has
//! nothing to poll it with while a CPU-bound pass holds that thread — on such
//! a machine nothing else the runtime drives runs either, and the recorded
//! flag is then set at the next yield rather than at once.

use std::sync::atomic::{AtomicU8, Ordering};

/// A signal that ends a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    Interrupt,
    Terminate,
    /// The terminal this run was started from went away. Its default action
    /// ends the process too, and a run that does not listen for it dies with
    /// its slot still held (carrick#1235) — the same outcome a Ctrl-C had
    /// before either was listened for.
    Hangup,
}

impl Shutdown {
    pub fn name(self) -> &'static str {
        match self {
            Shutdown::Interrupt => "SIGINT",
            Shutdown::Terminate => "SIGTERM",
            Shutdown::Hangup => "SIGHUP",
        }
    }

    /// 128 plus the signal number, the code a shell reports for a process a
    /// signal ended, so a caller reading the status sees the same thing it
    /// would have without the handler. Every exit code a scan can end with is
    /// listed in `docs/reference/runner.md`.
    pub fn exit_code(self) -> i32 {
        match self {
            Shutdown::Interrupt => 130,
            Shutdown::Terminate => 143,
            Shutdown::Hangup => 129,
        }
    }

    /// The encoding [`RECORDED`] holds. Zero is "no signal", so every signal
    /// is its position plus one.
    fn code(self) -> u8 {
        match self {
            Shutdown::Interrupt => 1,
            Shutdown::Terminate => 2,
            Shutdown::Hangup => 3,
        }
    }

    fn from_code(code: u8) -> Option<Shutdown> {
        match code {
            1 => Some(Shutdown::Interrupt),
            2 => Some(Shutdown::Terminate),
            3 => Some(Shutdown::Hangup),
            _ => None,
        }
    }
}

/// The exit code of a run that stopped because its type checker died of
/// memory, with `CARRICK_FAIL_ON_TYPE_MEMORY` set (carrick#2208): a runner
/// reads it as "run this again on a larger machine". Listed with every other
/// exit code in `docs/reference/runner.md`.
pub const TYPES_OUT_OF_MEMORY_EXIT: i32 = 3;

/// The exit code of a run that stopped itself because one discovery step made
/// no progress for the stall limit (carrick#3766): see [`request_stall`]. The
/// cloud runner and orchestrator read it (carrick-cloud
/// `runner/image/src/exit-codes.ts` `STALLED`) and count it like their own
/// deadline, so a code change here is a change on both sides. Listed with
/// every other exit code in `docs/reference/runner.md`.
pub const STALLED_EXIT: i32 = 4;

/// The reason an interrupted run reports, on either failure event.
pub const INTERRUPTED_REASON: &str = "interrupted";

/// How long the run may take to end through the signal path after it stopped
/// itself, before the watch ends the process directly. Longer than
/// [`INTERRUPTION_REPORT_BUDGET`], so the report and the log go out first
/// whenever the signal path works.
const STALL_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Where a scan stopped itself: the file (repo-relative, empty for a stage
/// after the per-file loop), the step or stage, and how long it had run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub file: String,
    pub step: &'static str,
    pub secs: u64,
}

impl Stall {
    fn place(&self) -> String {
        if self.file.is_empty() {
            format!("in the {} step", self.step)
        } else {
            format!("on {} (step {})", self.file, self.step)
        }
    }

    /// The line the run ends with, in the terminal and the log.
    pub fn line(&self) -> String {
        format!(
            "Scan stopped: discovery made no progress {} for {} s",
            self.place(),
            self.secs
        )
    }

    /// The reason the run reports to the cloud, on the scan row. The cloud
    /// orchestrator copies it into its alert for exit [`STALLED_EXIT`].
    pub fn reason(&self) -> String {
        format!("stalled: no progress {} for {} s", self.place(), self.secs)
    }
}

/// The stall this run stopped itself on, if it did. Set once, by the
/// discovery watch (`crate::slow_step`), before it sends the signal.
static STALL: std::sync::OnceLock<Stall> = std::sync::OnceLock::new();

/// Whether [`watch`] has installed the handlers in this process: a run with
/// none would take SIGTERM's default action and say nothing.
static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The stall this run stopped itself on, if any.
pub fn stall() -> Option<&'static Stall> {
    STALL.get()
}

/// The code a run that ended on `signal` exits with: [`STALLED_EXIT`] when
/// the run sent that signal to itself on a stall, else the signal's own.
pub fn exit_code_for(signal: Shutdown) -> i32 {
    if stall().is_some() {
        STALLED_EXIT
    } else {
        signal.exit_code()
    }
}

/// The reason an interrupted run reports: the stall's when it stopped itself.
pub fn interruption_reason() -> String {
    stall().map_or_else(|| INTERRUPTED_REASON.to_string(), Stall::reason)
}

/// Stop this run because one discovery step made no progress (carrick#3766).
///
/// It ends the way a SIGTERM does, by sending one to this process: the run
/// reports `scan-failed` with [`Stall::reason`], ships its log, kills its
/// type sidecars and exits [`STALLED_EXIT`]. A stuck step never reaches a
/// [`check`], so it is the race in `main`, polled on another worker, that
/// hears it. If the process is still here after [`STALL_EXIT_GRACE`], or no
/// handler is installed, the line is logged, the sidecars are killed and the
/// process exits [`STALLED_EXIT`] directly.
///
/// Does nothing when the run is already ending on a signal of its own.
pub fn request_stall(stall: Stall) {
    if requested().is_some() || STALL.set(stall).is_err() {
        return;
    }
    let exit_now = || {
        if let Some(stall) = STALL.get() {
            tracing::error!("{}", stall.line());
        }
        crate::services::type_sidecar::kill_live_sidecars();
        std::process::exit(STALLED_EXIT);
    };
    #[cfg(unix)]
    let signalled = WATCHING.load(Ordering::Relaxed)
        // SAFETY: a signal to this process, whose handler `watch` installed.
        && unsafe { libc::kill(libc::getpid(), libc::SIGTERM) } == 0;
    #[cfg(not(unix))]
    let signalled = false;
    if !signalled {
        exit_now();
    }
    // Its own thread: the caller is the watch thread, which discovery joins
    // when a pass notices the signal and returns.
    let _ = std::thread::Builder::new()
        .name("stall-exit".to_string())
        .spawn(move || {
            std::thread::sleep(STALL_EXIT_GRACE);
            exit_now();
        });
}

/// The signal this process was sent, for the passes that cannot be racing
/// anything. Zero until one arrives.
///
/// Process-global for the reason [`crate::scan_stage`] is: the passes that
/// have to notice sit several call layers below the race, and one process is
/// one scan, so there is exactly one answer to keep.
static RECORDED: AtomicU8 = AtomicU8::new(0);

/// The error a pass returns when it notices the signal.
///
/// It is an ordinary run failure on the way out — the engine marks the scan
/// failed with this as the reason and ships the run's log — and `main` reads
/// [`requested`] to give the process the signal's exit code instead of 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted {
    pub signal: Shutdown,
}

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A run that stopped itself reports where, through the engine's own
        // failure branch too: the scan row keeps the first reason it is sent.
        match stall() {
            Some(stall) => f.write_str(&stall.reason()),
            None => write!(f, "{INTERRUPTED_REASON} by {}", self.signal.name()),
        }
    }
}

impl std::error::Error for Interrupted {}

/// Which signal this process was sent, if any.
pub fn requested() -> Option<Shutdown> {
    Shutdown::from_code(RECORDED.load(Ordering::Relaxed))
}

/// Stop here if this process was signalled.
///
/// Cheap enough to call between files and at every phase boundary, which is
/// where it belongs: a check inside the parser would be neither cheap nor
/// bounded, and a pass that has just finished a file is a pass that can be
/// abandoned without leaving half of one behind.
pub fn check() -> Result<(), Interrupted> {
    match requested() {
        Some(signal) => Err(Interrupted { signal }),
        None => Ok(()),
    }
}

/// Record a signal. The first one wins: it is the one the run reports and the
/// one whose code the process exits with, and a second is an instruction to go
/// now rather than a correction to what happened.
fn record(signal: Shutdown) {
    let _ = RECORDED.compare_exchange(0, signal.code(), Ordering::Relaxed, Ordering::Relaxed);
}

/// Listen for every signal that ends a scan, from now, in a task of this
/// process's own.
///
/// The handlers are registered here rather than in the task: registration has
/// to have happened before the analysis starts, and a task's first poll is
/// scheduled, not immediate. A signal that lands before a handler exists takes
/// its default action and reports nothing.
pub fn watch() -> ShutdownWatcher {
    let mut listener = ShutdownListener::install();
    WATCHING.store(true, Ordering::Relaxed);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let signal = listener.recv().await;
            record(signal);
            // The type sidecars go now, whatever the run does next: one busy
            // building a program hears neither this signal nor this process
            // exiting until that work ends, and was left running after the
            // scan that started it was gone. A pass waiting on one reads a
            // dead process and returns, which ends the run sooner.
            let killed = crate::services::type_sidecar::kill_live_sidecars();
            tracing::debug!("{} ended {killed} type sidecar(s)", signal.name());
            // Nobody is racing any more: this run is on its way out and the
            // recorded flag is what the passes read.
            if tx.send(signal).is_err() {
                break;
            }
        }
    });
    ShutdownWatcher { rx }
}

/// The signals the watcher heard, in the order they arrived.
pub struct ShutdownWatcher {
    rx: tokio::sync::mpsc::UnboundedReceiver<Shutdown>,
}

impl ShutdownWatcher {
    /// The next signal. Never resolves once the watcher is gone, which it only
    /// is when this receiver has been dropped.
    pub async fn recv(&mut self) -> Shutdown {
        match self.rx.recv().await {
            Some(signal) => signal,
            None => std::future::pending().await,
        }
    }

    /// Drop the signals that have already arrived, so what [`Self::recv`]
    /// answers next is a signal sent from here on, and say how many there
    /// were.
    ///
    /// The run that ends because a pass read [`check`] never consumed the
    /// signal that ended it. Racing a report against it unconsumed would
    /// abandon that report in its first poll — "a second Ctrl-C ends this
    /// process now" answered by the first one (carrick#1387).
    pub fn drain(&mut self) -> usize {
        let mut dropped = 0;
        while self.rx.try_recv().is_ok() {
            dropped += 1;
        }
        dropped
    }
}

/// SIGINT, SIGTERM and SIGHUP, listened for from the moment [`Self::install`]
/// runs.
struct ShutdownListener {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
}

impl ShutdownListener {
    /// Register every handler now. A signal that cannot be listened for is
    /// left with its default action.
    fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                interrupt: signal(SignalKind::interrupt()).ok(),
                terminate: signal(SignalKind::terminate()).ok(),
                hangup: signal(SignalKind::hangup()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// The next signal. Never resolves when none could be listened for.
    async fn recv(&mut self) -> Shutdown {
        #[cfg(unix)]
        {
            async fn next(stream: Option<&mut tokio::signal::unix::Signal>) {
                match stream {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            }
            tokio::select! {
                () = next(self.interrupt.as_mut()) => Shutdown::Interrupt,
                () = next(self.terminate.as_mut()) => Shutdown::Terminate,
                () = next(self.hangup.as_mut()) => Shutdown::Hangup,
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => Shutdown::Interrupt,
                Err(_) => std::future::pending().await,
            }
        }
    }
}

/// How long an interrupted run may spend telling the cloud it stopped.
///
/// Deliberately shorter than the request timeout the marker itself carries:
/// someone who pressed Ctrl-C is waiting for the process to go, and the whole
/// report — reading the credential, asking git for the remote on the pre-scan
/// branch, the POST — has to fit inside what they will wait for. The slot's
/// own expiry is what covers a report that does not fit (carrick#1235).
pub const INTERRUPTION_REPORT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// How an interrupted run's report to the cloud ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The report was made, whatever the cloud did with it.
    Reported,
    /// A second signal arrived while it was being made. Someone is asking
    /// this process to go now, and it goes.
    SecondSignal(Shutdown),
    /// [`INTERRUPTION_REPORT_BUDGET`] ran out first.
    Budget,
}

/// Run `report` with a way out of it: a second signal, or a budget.
///
/// The report is best effort on a process that is already ending, so nothing
/// here waits on it — both escapes abandon it where it stands. `biased`, so a
/// second signal that is already pending wins over a budget that expired in
/// the same poll: what the user did beats what the clock did.
pub async fn within_budget<R, S>(report: R, second_signal: S, budget: std::time::Duration) -> Ended
where
    R: std::future::Future<Output = ()>,
    S: std::future::Future<Output = Shutdown>,
{
    tokio::pin!(report, second_signal);
    tokio::select! {
        biased;
        signal = &mut second_signal => Ended::SecondSignal(signal),
        () = &mut report => Ended::Reported,
        () = tokio::time::sleep(budget) => Ended::Budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Every signal round-trips its encoding, and one no release wrote reads
    /// as no signal rather than as whichever one sits at that index.
    ///
    /// Nothing here writes [`RECORDED`], and nothing in this binary may: a
    /// test that sets the process-global is read by every other test in the
    /// process — two thousand of them, on parallel threads, some of which run
    /// pipeline stages that call [`check`] — as an interruption of its own.
    /// What the flag does under a real signal is proven where a real signal
    /// can be raised safely, which is one test per process in
    /// `tests/shutdown_signal_test.rs` and `tests/shutdown_drain_test.rs`.
    /// The stall's line and reason name the file and step, or the stage, and
    /// the reason starts with the token the cloud strips. Built from values
    /// only: nothing here sets [`STALL`] (see the note below on [`RECORDED`]).
    #[test]
    fn a_stall_names_its_place_in_the_line_and_the_reason() {
        let on_file = Stall {
            file: "src/render.ts".to_string(),
            step: "request summary",
            secs: 600,
        };
        assert_eq!(
            on_file.line(),
            "Scan stopped: discovery made no progress on src/render.ts (step request summary) for 600 s"
        );
        assert_eq!(
            on_file.reason(),
            "stalled: no progress on src/render.ts (step request summary) for 600 s"
        );
        let in_stage = Stall {
            file: String::new(),
            step: "call edges",
            secs: 601,
        };
        assert_eq!(
            in_stage.line(),
            "Scan stopped: discovery made no progress in the call edges step for 601 s"
        );
        assert_eq!(STALLED_EXIT, 4);
    }

    #[test]
    fn every_signal_round_trips_its_encoding() {
        for signal in [Shutdown::Interrupt, Shutdown::Terminate, Shutdown::Hangup] {
            assert_eq!(Shutdown::from_code(signal.code()), Some(signal));
            // Zero is "no signal", so no signal may encode to it.
            assert_ne!(signal.code(), 0);
        }
        assert_eq!(Shutdown::from_code(0), None);
        assert_eq!(Shutdown::from_code(200), None);
    }

    /// The reason the cloud stores names the signal, and still carries the
    /// word every interrupted run reports with.
    #[test]
    fn an_interruption_says_which_signal_ended_the_run() {
        let text = Interrupted {
            signal: Shutdown::Terminate,
        }
        .to_string();
        assert!(text.starts_with(INTERRUPTED_REASON), "{text}");
        assert!(text.contains("SIGTERM"), "{text}");
        assert_eq!(Shutdown::Interrupt.exit_code(), 130);
        assert_eq!(Shutdown::Terminate.exit_code(), 143);
        assert_eq!(Shutdown::Hangup.exit_code(), 129);
    }

    /// The report is made if it can be, and abandoned if it cannot: a user who
    /// pressed Ctrl-C is waiting on this process, not on a request about it.
    ///
    /// Every case is wrapped in a timeout an order of magnitude past the
    /// budget, because the failure this guards against is a report with no way
    /// out of it — which hangs rather than fails.
    #[tokio::test(start_paused = true)]
    async fn an_interrupted_run_abandons_a_report_it_cannot_finish() {
        let budget = Duration::from_secs(5);
        let outer = Duration::from_secs(300);

        let ended = tokio::time::timeout(
            outer,
            within_budget(
                std::future::pending(),
                std::future::pending::<Shutdown>(),
                budget,
            ),
        )
        .await
        .expect("a report nobody stops still ends on the budget");
        assert_eq!(ended, Ended::Budget);

        let ended = tokio::time::timeout(
            outer,
            within_budget(
                std::future::pending(),
                std::future::ready(Shutdown::Interrupt),
                budget,
            ),
        )
        .await
        .expect("a second Ctrl-C ends it at once");
        assert_eq!(ended, Ended::SecondSignal(Shutdown::Interrupt));

        let ended = tokio::time::timeout(
            outer,
            within_budget(
                std::future::ready(()),
                std::future::pending::<Shutdown>(),
                budget,
            ),
        )
        .await
        .expect("a report that lands, lands");
        assert_eq!(ended, Ended::Reported);
    }
}
