//! Keeping this machine awake while a scan runs (carrick#1889).
//!
//! A first index of a large workspace runs for tens of minutes, and a busy
//! process does not stop a laptop from idle-sleeping. A machine that sleeps
//! mid-scan makes no call and says nothing, and from the cloud that looks
//! exactly like a scan doing local work. So a scan holds the platform's "do
//! not idle-sleep" assertion from the point it commits to running until it
//! ends, says so once, and can be told not to.
//!
//! **The hold cannot outlive the scan, and that is the helper's property
//! rather than this module's.** On macOS the assertion is held by
//! `caffeinate -i -w <pid>`, where the pid is this process's own. The helper
//! watches that pid and exits when it goes, however it goes: a return, a
//! `process::exit`, a signal, a panic, a `kill -9`. The scanner leaves through
//! `process::exit` on most of its paths, which runs no destructor, so nothing
//! here may depend on one. [`Hold`]'s `Drop` stops the helper early on the
//! paths that do unwind, and that is tidiness: the release is the watched pid
//! going away. A helper started for a pid that is already gone exits at once.
//!
//! `-i` is idle sleep only. The display still sleeps, and closing the lid
//! still sleeps the machine, which is what a person who closes a lid means.
//!
//! **The hold has a ceiling.** "As long as the scan's process" is only as
//! good as the process, and a process can hang. So the helper is also given
//! [`CEILING`] (`-t`), and lets go at whichever comes first. A scan still
//! running then says once that the machine may sleep again.
//!
//! **One hold per scan the user started.** A build (`index`, `resume`,
//! `refresh`) holds for its whole length and tells every scan it spawns that
//! it does ([`OFF_ENV`]), so a workspace of ten repos starts one helper and
//! says one line. A bare `carrick <path>` holds for itself.
//!
//! **Nothing is held, and nothing is said,** when the run was told not to
//! ([`OFF_FLAG`], [`OFF_ENV`]), on a CI runner, on a platform this module
//! knows no helper for (Linux and Windows today), or when the helper is not
//! there to run. A scan never fails over any of them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::debug;

/// The flag that lets the machine sleep during a scan.
pub const OFF_FLAG: &str = "--no-keep-awake";

/// The same choice as a variable, for a machine that should never be held, and
/// what a build sets on the scans it spawns: the build holds for all of them.
pub const OFF_ENV: &str = "CARRICK_NO_KEEP_AWAKE";

/// Set by `index --detach` on the build it starts, when the command the user
/// is reading has already said [`LINE`]. The detached build writes to a log
/// file, and the line belongs where the user is looking, once.
pub const SAID_ENV: &str = "CARRICK_KEEP_AWAKE_SAID";

/// The program that holds the assertion, run as `<program> -i -w <pid> -t
/// <seconds>`, in place of the one this platform is known to ship. What a
/// test points at a stand-in that records what it was asked.
pub const HELPER_ENV: &str = "CARRICK_KEEP_AWAKE_HELPER";

/// Part of macOS since 10.8, on the system volume, so there is nothing to
/// install and no permission to ask for.
const MACOS_HELPER: &str = "/usr/bin/caffeinate";

/// The longest a scan keeps the machine awake.
///
/// The hold lasts as long as the scan's process, and a process can hang: a
/// detached scan stuck on a wait would keep somebody's laptop awake overnight,
/// which is worse than the sleep this module is here to prevent. Three hours
/// is well above the longest real scan seen (100 minutes) and the 30 to 45
/// minutes a large first index is expected to take, so a scan that reaches it
/// is far likelier to be stuck than to be working.
///
/// The helper enforces it, so it holds for a process too stuck to act on
/// anything. [`LINE`] and [`CEILING_LINE`] state it in words, and a test holds
/// the three to one another.
pub const CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// What a scan says once, where the user is looking, when it holds.
pub const LINE: &str = "Keeping this machine awake until the scan finishes, for at most three \
                        hours. To let it sleep, pass --no-keep-awake or set \
                        CARRICK_NO_KEEP_AWAKE=1.";

/// What a scan that is still running says once, when [`CEILING`] has passed.
pub const CEILING_LINE: &str = "This scan has run for three hours, so the machine is no longer \
                                kept awake and may sleep.";

/// Whether [`OFF_FLAG`] was passed to this process.
static OFF: AtomicBool = AtomicBool::new(false);

/// Record that this process was given [`OFF_FLAG`]. Called once, from `main`,
/// which reads the flag off the argument list for every command that scans.
pub fn turn_off(flag: bool) {
    if flag {
        OFF.store(true, Ordering::Relaxed);
    }
}

/// Whether this process was given [`OFF_FLAG`], for the command that has to
/// hand it to a build it starts.
pub fn off_by_flag() -> bool {
    OFF.load(Ordering::Relaxed)
}

/// Everything the decision reads: what this run was told, and where it runs.
///
/// Carried as a value so the decision is a function of it, and a test states
/// each case without touching the process environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    /// [`OFF_FLAG`] was passed.
    pub off_flag: bool,
    /// [`OFF_ENV`] is set.
    pub off_env: bool,
    /// A CI runner: nobody is waiting at it, and its log is not the place for
    /// a line about a laptop.
    pub in_ci: bool,
    /// [`SAID_ENV`] is set: the line has been said for this scan already.
    pub said: bool,
    /// [`HELPER_ENV`], when set.
    pub helper: Option<PathBuf>,
    /// This build runs on macOS.
    pub macos: bool,
}

impl Asked {
    /// What this process was told.
    pub fn of_this_process() -> Self {
        Self {
            off_flag: off_by_flag(),
            off_env: is_set(OFF_ENV),
            in_ci: is_set("CI") || std::env::var_os("GITHUB_ACTIONS").is_some(),
            said: is_set(SAID_ENV),
            helper: std::env::var_os(HELPER_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// A variable set to anything but nothing, `0` or `false`, which is how every
/// on/off variable this binary reads is spelled.
fn is_set(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.is_empty() && value != "0" && value != "false")
        .unwrap_or(false)
}

/// Whether a run holds, and with what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Hold, with this program.
    Hold(PathBuf),
    /// Hold nothing and say nothing, and why, for the run's own log.
    Nothing(&'static str),
}

/// Decide whether a run that was asked this holds.
///
/// The opt-outs come first, so a run told not to hold never reaches the
/// question of how it would have.
pub fn decide(asked: &Asked) -> Decision {
    if asked.off_flag {
        return Decision::Nothing("turned off by --no-keep-awake");
    }
    if asked.off_env {
        return Decision::Nothing(
            "CARRICK_NO_KEEP_AWAKE is set, by the user or by the build that holds for this scan",
        );
    }
    if asked.in_ci {
        return Decision::Nothing("this is a CI runner");
    }
    match (&asked.helper, asked.macos) {
        (Some(helper), _) => Decision::Hold(helper.clone()),
        (None, true) => Decision::Hold(PathBuf::from(MACOS_HELPER)),
        (None, false) => Decision::Nothing("no helper is known for this platform"),
    }
}

/// What the helper is run with: idle sleep only, for as long as `pid` lives
/// and no longer than [`CEILING`], whichever ends first.
pub fn helper_args(pid: u32) -> [String; 5] {
    [
        "-i".to_string(),
        "-w".to_string(),
        pid.to_string(),
        "-t".to_string(),
        CEILING.as_secs().to_string(),
    ]
}

/// What ends a hold early. The watched pid going away ends it regardless.
pub type Release = Box<dyn FnOnce() + Send>;

/// The hold itself, for as long as this value lives.
///
/// Dropping it stops the helper at once. Not dropping it, because the process
/// left through `process::exit` or was killed, leaves the helper to notice
/// that the pid it watches has gone, which is the path that cannot be skipped.
pub struct Hold {
    release: Option<Release>,
    /// Dropped with the hold, which is what tells the watcher of the ceiling
    /// that the scan ended first and there is nothing to say.
    ceiling: Option<Sender<()>>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// Hold for this process from now, and say so through `say` when a hold was
/// taken and nobody has said it for this scan yet.
///
/// `say` is the caller's because where the user is looking is the caller's to
/// know: a build prints to stdout ahead of its first phase, which is the one
/// stream the npm renderer shows as it stands, and a bare scan logs it beside
/// its other lines.
pub fn begin(say: impl FnOnce(&str)) -> Option<Hold> {
    let mut hold = begin_with(
        &Asked::of_this_process(),
        std::process::id(),
        start_helper,
        say,
    )?;
    // Said the way a scan says why it is slow: logged, which a terminal and
    // the run's log both show, and crossed to a parent that is rendering this
    // run, which puts it beside the phase it is drawing.
    let (ended, _watcher) = say_at_ceiling(CEILING, || crate::progress::announce(CEILING_LINE));
    hold.ceiling = Some(ended);
    Some(hold)
}

/// Say that the ceiling has passed, once, unless the hold ends first.
///
/// The helper lets go at the ceiling on its own; this only keeps the words
/// true, so a scan that said "until the scan finishes" and is still running
/// says that the machine may sleep again. Dropping the returned sender is the
/// hold ending, and ends the wait with nothing said.
///
/// A thread rather than a timer on the runtime: a build runs on a blocking
/// thread and a scan's passes keep the runtime's own for minutes at a time,
/// and neither stops a parked thread from waking.
fn say_at_ceiling(
    ceiling: Duration,
    say: impl FnOnce() + Send + 'static,
) -> (Sender<()>, Option<JoinHandle<()>>) {
    let (ended, wait) = std::sync::mpsc::channel::<()>();
    let watcher = std::thread::Builder::new()
        .name("keep-awake-ceiling".to_string())
        .spawn(move || {
            if wait.recv_timeout(ceiling) == Err(RecvTimeoutError::Timeout) {
                say();
            }
        })
        .ok();
    (ended, watcher)
}

/// [`begin`], with what was asked, the pid to hold for and the platform call
/// passed in, so each can be stated by a test and the call observed.
pub fn begin_with<R>(asked: &Asked, pid: u32, request: R, say: impl FnOnce(&str)) -> Option<Hold>
where
    R: FnOnce(&Path, u32) -> std::io::Result<Release>,
{
    let helper = match decide(asked) {
        Decision::Hold(helper) => helper,
        Decision::Nothing(why) => {
            debug!("Not keeping this machine awake: {why}");
            return None;
        }
    };
    match request(&helper, pid) {
        Ok(release) => {
            // Only once the hold is real: a line about a hold that was not
            // taken is a promise nobody is keeping.
            if !asked.said {
                say(LINE);
            }
            Some(Hold {
                release: Some(release),
                ceiling: None,
            })
        }
        Err(error) => {
            debug!(
                "Not keeping this machine awake: could not run {}: {error}",
                helper.display()
            );
            None
        }
    }
}

/// Whether a scan started from this process would hold: the decision, and a
/// helper that is there to run.
///
/// For `index --detach`, which answers and exits while the build it started
/// does the holding, and still has to be the one that says so.
pub fn planned() -> bool {
    matches!(decide(&Asked::of_this_process()), Decision::Hold(helper) if helper.is_file())
}

/// Start the helper watching `pid`.
///
/// Its streams are closed rather than inherited: a helper holding this
/// process's stderr would keep a parent that reads that pipe to its end
/// waiting on the helper instead of on the scan.
fn start_helper(helper: &Path, pid: u32) -> std::io::Result<Release> {
    let mut child = Command::new(helper)
        .args(helper_args(pid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    debug!(
        "Keeping this machine awake: {} (pid {}) holds until pid {pid} exits, for at most {}s",
        helper.display(),
        child.id(),
        CEILING.as_secs()
    );
    Ok(Box::new(move || {
        let _ = child.kill();
        let _ = child.wait();
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the platform was asked, in the order it was asked.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Requested { helper: PathBuf, pid: u32 },
        Released,
    }

    /// A platform that records instead of holding.
    fn recording(
        calls: &Arc<Mutex<Vec<Call>>>,
    ) -> impl FnOnce(&Path, u32) -> std::io::Result<Release> {
        let calls = Arc::clone(calls);
        move |helper, pid| {
            calls.lock().unwrap().push(Call::Requested {
                helper: helper.to_path_buf(),
                pid,
            });
            let calls = Arc::clone(&calls);
            Ok(Box::new(move || calls.lock().unwrap().push(Call::Released)))
        }
    }

    fn on_a_mac() -> Asked {
        Asked {
            macos: true,
            ..Asked::default()
        }
    }

    /// The default: a scan on a Mac holds with the system's own helper, and
    /// asks it for idle sleep only, tied to the pid it was given, for no
    /// longer than the ceiling.
    #[test]
    fn a_scan_on_a_mac_holds_with_the_system_helper() {
        assert_eq!(
            decide(&on_a_mac()),
            Decision::Hold(PathBuf::from("/usr/bin/caffeinate"))
        );
        assert_eq!(helper_args(4242), ["-i", "-w", "4242", "-t", "10800"]);
    }

    /// The ceiling is three hours, and the two lines that state it in words
    /// state that number: changing one without the others fails here.
    #[test]
    fn the_ceiling_is_three_hours_and_both_lines_say_so() {
        assert_eq!(CEILING, Duration::from_secs(3 * 60 * 60));
        assert!(LINE.contains("for at most three hours"), "{LINE}");
        assert!(CEILING_LINE.contains("three hours"), "{CEILING_LINE}");
        assert!(!CEILING_LINE.contains('\n'), "one line");
    }

    /// A scan still running when the ceiling passes is told, once, that the
    /// machine may sleep again.
    #[test]
    fn a_scan_still_running_at_the_ceiling_says_so_once() {
        let said = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&said);
        let (still_held, watcher) = say_at_ceiling(Duration::from_millis(20), move || {
            *counted.lock().unwrap() += 1;
        });
        watcher
            .expect("a thread to watch the ceiling")
            .join()
            .expect("the watcher ends");
        assert_eq!(*said.lock().unwrap(), 1);
        drop(still_held);
        assert_eq!(*said.lock().unwrap(), 1, "and not again when the hold goes");
    }

    /// A scan that ends before the ceiling says nothing about it: the hold
    /// going is what ends the wait.
    #[test]
    fn a_scan_that_ends_before_the_ceiling_says_nothing_about_it() {
        let said = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&said);
        let (held, watcher) = say_at_ceiling(Duration::from_secs(3600), move || {
            *counted.lock().unwrap() += 1;
        });
        drop(held);
        watcher
            .expect("a thread to watch the ceiling")
            .join()
            .expect("the watcher ends when the hold does, not an hour later");
        assert_eq!(*said.lock().unwrap(), 0);
    }

    /// Each way of saying no holds nothing, whatever else is true.
    #[test]
    fn a_run_told_not_to_hold_holds_nothing() {
        for asked in [
            Asked {
                off_flag: true,
                ..on_a_mac()
            },
            Asked {
                off_env: true,
                ..on_a_mac()
            },
            Asked {
                in_ci: true,
                ..on_a_mac()
            },
            // A named helper does not override an opt-out either.
            Asked {
                off_env: true,
                helper: Some(PathBuf::from("/opt/helper")),
                ..on_a_mac()
            },
        ] {
            assert!(
                matches!(decide(&asked), Decision::Nothing(_)),
                "{asked:?} should hold nothing"
            );
        }
    }

    /// Linux and Windows: no helper is known, so nothing is held. A named
    /// helper is what would change that.
    #[test]
    fn a_platform_with_no_known_helper_holds_nothing() {
        assert!(matches!(decide(&Asked::default()), Decision::Nothing(_)));
        assert_eq!(
            decide(&Asked {
                helper: Some(PathBuf::from("/opt/helper")),
                ..Asked::default()
            }),
            Decision::Hold(PathBuf::from("/opt/helper"))
        );
    }

    /// The hold is requested once, for the pid it was given, the line is said
    /// once, and the hold is released when the scan's value goes.
    #[test]
    fn a_scan_requests_one_hold_says_so_once_and_releases_it_at_the_end() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut said = Vec::new();

        let hold = begin_with(&on_a_mac(), 4242, recording(&calls), |line| {
            said.push(line.to_string())
        });

        assert!(hold.is_some());
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Requested {
                helper: PathBuf::from("/usr/bin/caffeinate"),
                pid: 4242
            }]
        );
        assert_eq!(said, [LINE]);

        drop(hold);
        assert_eq!(calls.lock().unwrap().last(), Some(&Call::Released));
        assert_eq!(calls.lock().unwrap().len(), 2, "one request, one release");
    }

    /// A scan that fails releases on its way out: the hold is a value of the
    /// function that ran the scan, and an early return drops it.
    #[test]
    fn a_scan_that_fails_releases_its_hold() {
        fn a_failing_scan(calls: &Arc<Mutex<Vec<Call>>>) -> Result<(), String> {
            let _awake = begin_with(&on_a_mac(), 7, recording(calls), |_| {});
            Err("the scan failed".to_string())
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        assert!(a_failing_scan(&calls).is_err());
        assert_eq!(calls.lock().unwrap().last(), Some(&Call::Released));
    }

    /// And one that panics releases as the panic unwinds through it.
    #[test]
    fn a_scan_that_panics_releases_its_hold() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let inside = Arc::clone(&calls);
        let panicked = std::panic::catch_unwind(move || {
            let _awake = begin_with(&on_a_mac(), 7, recording(&inside), |_| {});
            panic!("the scan panicked");
        });
        assert!(panicked.is_err());
        assert_eq!(calls.lock().unwrap().last(), Some(&Call::Released));
    }

    /// An opt-out is an opt-out of the line too: nothing is asked of the
    /// platform and nothing is said.
    #[test]
    fn a_run_that_holds_nothing_asks_nothing_and_says_nothing() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut said = 0;
        let hold = begin_with(
            &Asked {
                off_flag: true,
                ..on_a_mac()
            },
            7,
            recording(&calls),
            |_| said += 1,
        );
        assert!(hold.is_none());
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(said, 0);
    }

    /// A helper that is not there to run costs the scan nothing: no hold, no
    /// line, and no failure.
    #[test]
    fn a_missing_helper_holds_nothing_and_says_nothing() {
        let mut said = 0;
        let hold = begin_with(
            &on_a_mac(),
            7,
            |_, _| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            |_| said += 1,
        );
        assert!(hold.is_none());
        assert_eq!(said, 0);
    }

    /// A detached build holds, and leaves the line to the command that
    /// started it and already said it.
    #[test]
    fn a_build_whose_starter_said_the_line_holds_without_repeating_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut said = 0;
        let hold = begin_with(
            &Asked {
                said: true,
                ..on_a_mac()
            },
            7,
            recording(&calls),
            |_| said += 1,
        );
        assert!(hold.is_some());
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(said, 0);
    }

    /// The line names both ways of turning the hold off, spelled as the
    /// parser and the environment read them.
    #[test]
    fn the_line_names_the_flag_and_the_variable() {
        assert!(LINE.contains(OFF_FLAG), "{LINE}");
        assert!(LINE.contains(&format!("{OFF_ENV}=1")), "{LINE}");
        assert!(!LINE.contains('\n'), "one line");
    }
}
