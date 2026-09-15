use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tracing::info;
use tracing_appender::rolling::{self, RollingFileAppender, RollingWriter};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Byte offset into today's log file at which *this run* started writing.
/// Captured during `init()` and used by `get_run_log_offset()` so log uploads
/// only ship the current run's content, not the day's accumulated tail (which
/// could include unrelated repos analyzed earlier on the same machine).
static RUN_START_OFFSET: OnceLock<u64> = OnceLock::new();

/// UUID v4 generated once per scanner invocation. Sent on every cloud
/// request as `X-Carrick-Run-Id` and logged in the run preamble so the
/// same key joins customer-side and CloudWatch logs for one scan.
static RUN_ID: OnceLock<String> = OnceLock::new();

/// Set by a process that drives scans as subprocesses, so the whole build is
/// one run: `carrick index` runs a scan per repo and a join, and each of them
/// minting its own id printed a second "Carrick run starting" banner with a
/// different key and left the cloud's logs unable to join them (carrick#997
/// item 2). Internal, like [`crate::progress::PROGRESS_ENV`]; nothing asks a
/// user to set it.
pub const RUN_ID_ENV: &str = "CARRICK_RUN_ID";

/// What this process is inside that run, for its own banner. Set beside
/// [`RUN_ID_ENV`]; absent in the process a user started.
pub const RUN_PHASE_ENV: &str = "CARRICK_RUN_PHASE";

/// Stable identifier for this scanner run. Taken from the parent that drives
/// this one when there is one, else a fresh UUID v4 on first call, then the
/// same string for the rest of the process.
pub fn run_id() -> &'static str {
    RUN_ID.get_or_init(|| {
        std::env::var(RUN_ID_ENV)
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    })
}

/// Initialize the global tracing subscriber with two layers:
///
/// 1. **Terminal layer** (stderr): Shows `INFO` by default, `DEBUG` with `--verbose`.
///    Uses a minimal format without timestamps or targets for a clean look.
///
/// 2. **File layer** (best effort): when `~/.carrick/logs/` is writable, appends
///    [`FILE_FILTER`] logs with timestamps to `carrick.log.YYYY-MM-DD` (daily
///    rotation, [`RETAINED_LOG_DAYS`] days kept). If the directory can't be
///    created the file layer is skipped and only the terminal layer is active
///    — in that case the run preamble only reaches stderr.
pub fn init(verbose: bool) {
    let terminal_filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    let terminal_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_level(false)
        .without_time()
        .with_filter(terminal_filter);

    // Try to set up file logging to ~/.carrick/logs/
    let log_dir = dirs::home_dir().map(|h| h.join(".carrick").join("logs"));

    // Daily rotation with a retention cap. Rotation alone bounded nothing —
    // nothing deleted an old file, and 143 GB of them filled a disk
    // mid-session (carrick#741). The file name is unchanged,
    // `carrick.log.<date>` exactly as `get_log_file_path` computes it, so the
    // run-log upload still finds today's file and ships this run's bytes from
    // RUN_START_OFFSET.
    let cap = log_size_cap();
    // Reported after the subscriber exists, so the fresh file says why it
    // starts where it does rather than a reader finding a log that begins
    // mid-scan.
    let mut rolled = None;
    let file_appender = log_dir.as_ref().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        // Roll before the offset is taken: a run that starts on a file already
        // at the cap starts on an empty one, so the log you want — this run's —
        // is the one that fits.
        rolled = roll_over_cap(dir, &today(), cap);
        // Capture the current size of today's log file *before* we write
        // anything. Anything past this offset belongs to this run.
        let _ = RUN_START_OFFSET.set(current_log_file_size());
        let inner = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir)
            .ok()?;
        Some(CappedAppender::new(
            inner,
            budget(cap, RUN_START_OFFSET.get().copied().unwrap_or(0)),
        ))
    });

    if let Some(file_appender) = file_appender {
        let file_layer = fmt::layer()
            .with_writer(file_appender)
            .with_ansi(false)
            .with_target(true)
            .with_filter(EnvFilter::new(FILE_FILTER));

        let _ = tracing_subscriber::registry()
            .with(terminal_layer)
            .with(file_layer)
            .try_init();
        emit_run_preamble();
        if let Some(bytes) = rolled {
            info!(
                "Today's Carrick debug log had reached {:.0} MB (cap {} MB), so it was moved to \
                 carrick.log.<date>{} and this run starts a new one.",
                bytes as f64 / (1024.0 * 1024.0),
                cap / (1024 * 1024),
                ROLLED_SUFFIX
            );
        }
        warn_if_log_is_large();
        return;
    }

    // Fallback: terminal only
    let _ = tracing_subscriber::registry()
        .with(terminal_layer)
        .try_init();
    emit_run_preamble();
}

/// What the file layer writes: this crate at `debug`, everything else at
/// `info`.
///
/// It was a bare `debug`, which meant every dependency's debug lines too —
/// reqwest and hyper narrate each connection, its pool and its headers, and
/// those lines are the reason a laptop's log could not be shipped without
/// reading it first. Our own `debug` is what the file is for: stage names,
/// counts, timings and code identifiers. The terminal layer is unchanged; it
/// has its own filter and shows `info` (or `debug` with `--verbose`).
const FILE_FILTER: &str = "info,carrick=debug";

/// Rewrite one log line for upload, or drop it.
///
/// Two rules, and both are about what a line may carry off the machine
/// (carrick#1063):
///
/// 1. The user's home directory becomes `~`. A laptop's paths carry the
///    account name and often the employer's directory layout, and none of it
///    identifies a run — the repo and the scanner version already do.
/// 2. A line naming a credential goes entirely. `Authorization` is the header
///    this scanner sends, `X-Amz-Signature` rides every presigned URL it is
///    handed, and `carrick_sk_` is the literal prefix of the credential
///    itself. None of these are logged today; the rule is what keeps a line
///    added later from shipping one before anyone notices.
///
/// Whole lines rather than a surgical edit: a line naming a credential is a
/// line whose value is already suspect, and a debug log is never worth
/// deciding that question on.
pub fn redact_log_line(line: &str, home: Option<&str>) -> Option<String> {
    const SECRET_MARKERS: [&str; 3] = ["Authorization", "X-Amz-Signature", "carrick_sk_"];
    if SECRET_MARKERS.iter().any(|marker| line.contains(marker)) {
        return None;
    }
    Some(redact_home_in(line, home))
}

/// The home directory this process runs under, as the path spelling a log line
/// would carry. `None` when there is no home to redact, which is most of CI.
pub fn home_for_redaction() -> Option<String> {
    // `dirs::home_dir` is what `log_dir` already resolves the log directory
    // with, so the spelling this replaces is the spelling that gets written.
    let home = dirs::home_dir()?;
    let home = home.to_str()?.trim_end_matches('/').to_string();
    // `/` as a home would turn every absolute path into `~...`, and a one-
    // character home is not a home worth hiding.
    (home.len() > 1).then_some(home)
}

/// Replace `home` with `~` wherever it appears in `text`.
///
/// Shared by the run-log upload and the fail marker's `reason`, so a failure
/// and the log that explains it redact the same way.
pub fn redact_home_in(text: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() => text.replace(home, "~"),
        _ => text.to_string(),
    }
}

/// [`redact_log_line`] over a whole log slice, dropping the lines it drops.
pub fn redact_log(content: &str) -> String {
    let home = home_for_redaction();
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        if let Some(kept) = redact_log_line(line, home.as_deref()) {
            out.push_str(&kept);
            out.push('\n');
        }
    }
    out
}

/// Bytes one debug log file may hold.
///
/// Days were the only bound until carrick#836, and days measure the wrong
/// thing: the file's size depends on how much anyone scanned that day, so an
/// ordinary day of local runs wrote 9.3 GB into one file and three days of
/// them was 25 GB on a laptop. Two hundred megabytes is far more DEBUG than
/// anyone reads, and with it the directory holds at most
/// [`RETAINED_LOG_DAYS`] files of the appender's plus the one generation this
/// module rolls aside: about 800 MB, whatever anyone scans.
///
/// The appender's own pruning matches on the `carrick.log` prefix, so whether
/// it counts a rolled `carrick.log.<date>.1` toward its cap depends on the
/// platform (it sorts by creation time where the filesystem reports one, and
/// falls back to parsing the date out of the name, which a rolled name defeats).
/// Either way the count is bounded, which is what the file names cannot be
/// relied on to say and what the test below asserts.
const LOG_SIZE_CAP_BYTES: u64 = 200 * 1024 * 1024;

/// Raise, lower or remove the byte cap for one run. `0` removes it, which is
/// what to set when a scan has to be debugged past 200 MB of its own output.
const LOG_SIZE_CAP_ENV: &str = "CARRICK_LOG_MAX_MB";

/// The suffix a rolled generation of today's file carries.
const ROLLED_SUFFIX: &str = ".1";

/// The byte cap this run writes under.
fn log_size_cap() -> u64 {
    match std::env::var(LOG_SIZE_CAP_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => u64::MAX,
            Ok(mb) => mb.saturating_mul(1024 * 1024),
            Err(_) => LOG_SIZE_CAP_BYTES,
        },
        Err(_) => LOG_SIZE_CAP_BYTES,
    }
}

/// What this run may still write to a file that already holds `existing`.
fn budget(cap: u64, existing: u64) -> u64 {
    if cap == u64::MAX {
        return u64::MAX;
    }
    cap.saturating_sub(existing)
}

/// The day this run's file is named for, in UTC.
///
/// One clock, and it is the appender's (carrick#1052). `tracing_appender`
/// names `carrick.log.<date>` from `Utc::now()` and nothing here can tell it
/// otherwise, so every reader of that name has to use the same clock or it
/// reads a file that does not exist. This is the reader: the roll at startup,
/// the offset the upload slices from, and [`get_log_file_path`] all go through
/// it. With `Local` here, a machine west of UTC spent the hours between local
/// midnight and 00:00 UTC computing yesterday's name for today's file — the
/// roll looked at nothing, and the run-log upload found nothing to ship, which
/// is the window a first index was lost in.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Move today's file aside when it is already at the cap, and keep exactly one
/// rolled generation in the directory. Returns the bytes moved.
///
/// The rolled name keeps the day in it (`carrick.log.<date>.1`) so a reader can
/// tell what it is, and every other rolled file goes: without that, a machine
/// scanning every day would accumulate one per day and the byte bound would be
/// a function of usage again, which is the bug.
///
/// `carrick index` runs one child process per repo and each one comes through
/// here, so a workspace whose repos write more than the cap between them rolls
/// mid-index and the earlier repos' lines go. At 200 MB a repo that reaches it
/// has already written more DEBUG than anyone reads; the run says when it
/// happened, and `CARRICK_LOG_MAX_MB=0` is there for the run that needs it all.
fn roll_over_cap(dir: &Path, day: &str, cap: u64) -> Option<u64> {
    let live = dir.join(format!("carrick.log.{day}"));
    // Built by hand rather than with `with_extension`, which would read the
    // date as the extension and write `carrick.log.1`.
    let rolled = dir.join(format!("carrick.log.{day}{ROLLED_SUFFIX}"));
    let size = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
    let over = size >= cap;
    for existing in rolled_files(dir) {
        // The one we are about to write is replaced; the rest are other days'.
        if !over && existing == rolled {
            continue;
        }
        let _ = std::fs::remove_file(existing);
    }
    if !over {
        return None;
    }
    std::fs::rename(&live, &rolled).ok().map(|()| size)
}

fn rolled_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("carrick.log.") && name.ends_with(ROLLED_SUFFIX)
                    })
        })
        .collect()
}

/// The rolling appender, with a byte budget for this run.
///
/// Rolling aside at startup bounds what earlier runs left behind; this bounds
/// what one run can write, which is the other half — a single large scan is
/// what produced the gigabytes. Past the budget the lines are dropped rather
/// than the run failing: a debug log is never worth ending a scan for. One
/// line on stderr says it happened and how to turn the cap off.
struct CappedAppender {
    inner: RollingFileAppender,
    remaining: AtomicU64,
    announced: AtomicBool,
}

impl CappedAppender {
    fn new(inner: RollingFileAppender, remaining: u64) -> Self {
        Self {
            inner,
            remaining: AtomicU64::new(remaining),
            announced: AtomicBool::new(false),
        }
    }
}

impl<'a> MakeWriter<'a> for CappedAppender {
    type Writer = CappedWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        CappedWriter {
            inner: self.inner.make_writer(),
            remaining: &self.remaining,
            announced: &self.announced,
        }
    }
}

struct CappedWriter<'a> {
    inner: RollingWriter<'a>,
    remaining: &'a AtomicU64,
    announced: &'a AtomicBool,
}

impl Write for CappedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.remaining.load(Ordering::Relaxed);
        if left == 0 {
            // Reported as written: a dropped debug line is not an error the
            // scanner should see, and returning 0 would look like a full disk.
            return Ok(buf.len());
        }
        // A whole event is written or none of it is, so the file overshoots by
        // at most one line rather than ending mid-record.
        let written = self.inner.write(buf)?;
        let now = left.saturating_sub(written as u64);
        self.remaining.store(now, Ordering::Relaxed);
        if now == 0 && !self.announced.swap(true, Ordering::Relaxed) {
            eprintln!(
                "Carrick's debug log for today reached its {} MB cap and the rest of this run is \
                 not being written to it. Set {}=0 to log without a cap.",
                log_size_cap() / (1024 * 1024),
                LOG_SIZE_CAP_ENV
            );
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// How many days of debug logs `~/.carrick/logs/` keeps.
///
/// The file layer writes at DEBUG whatever the terminal is set to, so a day
/// of scanning a large repo is measured in gigabytes and nothing but this cap
/// removes it: the appender's daily rotation only starts a new file. Three
/// days covers "what happened in the run I am asking about", which is all the
/// local copy is for — the cloud gets this run's slice at upload time.
const RETAINED_LOG_DAYS: usize = 3;

/// Size at which the log directory is worth mentioning: nothing about a
/// scanner says "check your home directory", and the first symptom of not
/// knowing was a disk that filled mid-run (carrick#741). It measures the
/// directory rather than one file, because one file is now capped.
const LOG_SIZE_NOTICE_BYTES: u64 = 1024 * 1024 * 1024;

/// Say where the logs are and how much of the disk they hold, once that is
/// enough that someone would want to know. Silent below the threshold, which
/// is every ordinary run.
fn warn_if_log_is_large() {
    let Some(dir) = log_dir() else {
        return;
    };
    let size = log_dir_size(&dir);
    if size < LOG_SIZE_NOTICE_BYTES {
        return;
    }
    tracing::warn!(
        "Carrick's debug logs hold {:.1} GB ({}). Each file is capped at {} MB and {} day(s) are \
         kept; delete the directory to reclaim the space now.",
        size as f64 / (1024.0 * 1024.0 * 1024.0),
        dir.display(),
        log_size_cap() / (1024 * 1024),
        RETAINED_LOG_DAYS
    );
}

fn log_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".carrick").join("logs"))
}

fn log_dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

fn current_log_file_size() -> u64 {
    get_log_file_path()
        .and_then(|p| std::fs::metadata(&p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Byte offset into the daily log file at which this run began. `None` if
/// the file layer wasn't initialized (terminal-only fallback).
pub fn get_run_log_offset() -> Option<u64> {
    RUN_START_OFFSET.get().copied()
}

/// Emit a structured preamble at the start of every run. Goes through `tracing`
/// so it lands in the file log (when available) and the terminal (info+).
/// This is the "what was the environment when this ran" record that makes
/// uploaded logs interpretable after the fact.
///
/// Intentionally omits absolute filesystem paths (e.g. cwd) — this preamble is
/// uploaded to S3 from local runs as well as CI, and workstation paths often
/// contain usernames or internal directory names that aren't needed to
/// identify a run. GitHub repo/sha are sufficient for CI; for local runs the
/// repository name from the carrick.json + scanner version are enough.
fn emit_run_preamble() {
    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| "<unset>".to_string())
    }

    // A process a parent drives says which part of that parent's run it is, so
    // the second banner in a build's output is readable as the phase it
    // belongs to rather than as a second run (carrick#997 item 2).
    if let Ok(phase) = std::env::var(RUN_PHASE_ENV)
        && !phase.trim().is_empty()
    {
        info!(
            run_id = run_id(),
            scanner_version = env!("CARGO_PKG_VERSION"),
            phase = %phase,
            "Carrick run continuing"
        );
        return;
    }

    info!(
        run_id = run_id(),
        scanner_version = env!("CARGO_PKG_VERSION"),
        api_endpoint = env!("CARRICK_API_ENDPOINT"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ci = %env("CI"),
        github_event = %env("GITHUB_EVENT_NAME"),
        github_ref = %env("GITHUB_REF"),
        github_repo = %env("GITHUB_REPOSITORY"),
        github_sha = %env("GITHUB_SHA"),
        github_run_id = %env("GITHUB_RUN_ID"),
        github_workflow = %env("GITHUB_WORKFLOW"),
        runner_os = %env("RUNNER_OS"),
        "Carrick run starting"
    );
}

/// Return the path to today's log file, if it exists.
///
/// The rolling appender creates files named `carrick.log.YYYY-MM-DD`.
pub fn get_log_file_path() -> Option<std::path::PathBuf> {
    let log_file = log_dir()?.join(format!("carrick.log.{}", today()));
    if log_file.exists() {
        Some(log_file)
    } else {
        None
    }
}

/// True when stderr is attached to a terminal (TTY). False under CI redirects
/// like `./carrick . > analysis.log 2>&1`, where indicatif renders nothing
/// and the user sees a long silent gap between stage transitions.
fn is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Create a spinner with a message. Call `finish_with_message` when done.
///
/// In non-TTY environments (CI, piped output) the animated spinner is
/// suppressed and a plain `▸ <msg>` line is emitted to stderr so the user
/// still sees the stage start. `finish_spinner` mirrors with `✓ <msg>`.
pub fn spinner(msg: &str) -> ProgressBar {
    if !is_tty() {
        eprintln!("▸ {}", msg);
        // Returning a hidden ProgressBar keeps the call sites uniform — no
        // ticks are drawn and `finish_*` skips its rendering path. The
        // matching `✓ <msg>` line is still emitted by the same is_tty()
        // gate inside `finish_spinner`.
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Say which item a running stage has reached, without finishing it.
///
/// A stage that loops over services is one spinner for the whole loop, and a
/// thirteen-service repo spent minutes on each without saying which
/// (carrick#1067). In a terminal that is the spinner's own label; in a pipe —
/// which is where a detached scan's log is read — it is one `▸ <msg>` line per
/// item, the same shape [`spinner`] emits for a stage start.
pub fn progress(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        eprintln!("▸ {}", msg);
        return;
    }
    pb.set_message(msg.to_string());
}

/// Finish a spinner with a success checkmark.
pub fn finish_spinner(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        eprintln!("✓ {}", msg);
        return;
    }
    pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
    pb.finish_with_message(format!("\x1b[32m✓\x1b[0m {}", msg));
}

/// Severity of a GitHub Actions annotation.
pub enum Annotation {
    Warning,
    Error,
}

/// Emit a GitHub Actions annotation for `msg`, when running in one.
///
/// The Action tees the scanner's own output and nothing else, so a line the
/// scanner does not print is a line the Action log does not carry. An
/// annotation additionally surfaces on the run summary and next to the step,
/// which is where someone looks when a scan finished but did less than it was
/// asked to. Outside Actions this prints nothing: the same fact reaches a
/// local run through the warning it accompanies.
pub fn annotate(level: Annotation, msg: &str) {
    if std::env::var("GITHUB_ACTIONS").is_err() {
        return;
    }
    let tag = match level {
        Annotation::Warning => "warning",
        Annotation::Error => "error",
    };
    // Annotations are one line: a newline would end the command and print the
    // rest as ordinary log text.
    eprintln!("::{}::{}", tag, msg.replace('\n', " "));
}

/// Finish a spinner with a warning marker.
pub fn finish_spinner_warn(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        eprintln!("⚠ {}", msg);
        return;
    }
    pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
    pb.finish_with_message(format!("\x1b[33m⚠\x1b[0m {}", msg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// The retention cap must not change the file name.
    ///
    /// `get_log_file_path` computes `carrick.log.<date>` by hand, and the
    /// run-log upload reads this run's bytes from an offset into that exact
    /// file. A builder that spelled the name differently would leave both
    /// pointing at nothing, and the upload would go quiet rather than fail.
    #[test]
    fn the_capped_appender_writes_the_name_the_upload_path_looks_for() {
        let dir = tempfile::tempdir().expect("temp dir");

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        // `today()`, not a second spelling of the clock: the point of the
        // assertion is that the name the upload path computes is the name the
        // appender wrote (carrick#1052).
        let expected = dir.path().join(format!("carrick.log.{}", today()));
        assert!(
            expected.is_file(),
            "expected {}, found {:?}",
            expected.display(),
            log_files(dir.path())
        );
    }

    /// The cap must delete, and it must do so on an ordinary run.
    ///
    /// If it only pruned when the clock crossed midnight, a machine already
    /// holding six days of logs would keep holding them however many scans it
    /// ran, which is the case carrick#741 was reported from.
    #[test]
    fn the_cap_deletes_the_days_it_is_past_on_the_first_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}")), "old\n")
                .expect("seed old log");
        }

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        let kept = log_files(dir.path());
        assert!(
            kept.len() <= RETAINED_LOG_DAYS,
            "kept {} files, cap is {}: {:?}",
            kept.len(),
            RETAINED_LOG_DAYS,
            kept
        );
        assert!(
            kept.contains(&format!("carrick.log.{}", today())),
            "today's file was pruned: {kept:?}"
        );
        assert!(
            !kept.contains(&"carrick.log.2026-09-01".to_string()),
            "the oldest file survived: {kept:?}"
        );
    }

    /// A file already at the cap is moved aside, so this run logs to an empty
    /// one. The bytes that filled it were earlier runs', and they are the ones
    /// worth losing.
    #[test]
    fn a_file_at_the_cap_is_rolled_aside_before_the_run_writes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("carrick.log.2026-09-08");
        std::fs::write(&live, vec![b'x'; 512]).expect("seed");

        roll_over_cap(dir.path(), "2026-09-08", 256);

        assert!(
            !live.exists(),
            "today's file was left in place: {:?}",
            log_files(dir.path())
        );
        assert_eq!(
            std::fs::read(dir.path().join("carrick.log.2026-09-08.1"))
                .expect("rolled file")
                .len(),
            512
        );
    }

    /// Under the cap nothing moves: an ordinary run must not lose the log of
    /// the run before it.
    #[test]
    fn a_file_under_the_cap_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("carrick.log.2026-09-08");
        std::fs::write(&live, vec![b'x'; 16]).expect("seed");

        assert_eq!(roll_over_cap(dir.path(), "2026-09-08", 256), None);

        assert_eq!(std::fs::read(&live).expect("today's file").len(), 16);
        assert!(
            log_files(dir.path())
                .iter()
                .all(|name| !name.ends_with(".1"))
        );
    }

    /// One rolled generation exists at a time. Keeping one per day would make
    /// the byte bound a function of how many days the machine scanned on,
    /// which is the thing carrick#836 is about.
    #[test]
    fn only_one_rolled_generation_survives_a_roll() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-05", "2026-09-06", "2026-09-07"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}.1")), "old\n")
                .expect("seed rolled");
        }
        std::fs::write(dir.path().join("carrick.log.2026-09-08"), vec![b'x'; 512])
            .expect("seed today");

        roll_over_cap(dir.path(), "2026-09-08", 256);

        let rolled: Vec<String> = log_files(dir.path())
            .into_iter()
            .filter(|name| name.ends_with(".1"))
            .collect();
        assert_eq!(rolled, vec!["carrick.log.2026-09-08.1".to_string()]);
    }

    /// The budget is what a run may add, not what the file may hold, so a file
    /// already part-full does not get a second full cap written into it.
    #[test]
    fn the_budget_is_the_cap_less_what_is_already_there() {
        assert_eq!(budget(1000, 0), 1000);
        assert_eq!(budget(1000, 400), 600);
        assert_eq!(budget(1000, 4000), 0);
        assert_eq!(budget(u64::MAX, 4000), u64::MAX);
    }

    /// Past the budget the writer drops lines and reports them written: a
    /// debug log is never worth failing a scan for, and a short write would
    /// read as a full disk.
    #[test]
    fn the_writer_stops_at_the_budget_without_failing_the_run() {
        let dir = tempfile::tempdir().expect("temp dir");
        let appender = CappedAppender::new(
            rolling::Builder::new()
                .rotation(rolling::Rotation::DAILY)
                .filename_prefix("carrick.log")
                .max_log_files(RETAINED_LOG_DAYS)
                .build(dir.path())
                .expect("build appender"),
            10,
        );

        let line = b"0123456789abcdef\n";
        for _ in 0..5 {
            let mut writer = appender.make_writer();
            assert_eq!(writer.write(line).expect("write"), line.len());
            writer.flush().expect("flush");
        }

        let written = std::fs::metadata(dir.path().join(format!("carrick.log.{}", today())))
            .expect("today's file")
            .len();
        // One event of overshoot is allowed; five are not.
        assert_eq!(written, line.len() as u64);
    }

    /// The cap is a number a user can change for one run, including off.
    #[test]
    fn the_cap_reads_the_environment() {
        // Parsed by the same function the run uses; the env var itself is
        // process-global, so this checks the arithmetic it does with it.
        assert_eq!(LOG_SIZE_CAP_BYTES, 200 * 1024 * 1024);
        assert_eq!(budget(log_size_cap(), 0), LOG_SIZE_CAP_BYTES);
    }

    /// The two prunings together leave a bounded number of files, whichever
    /// of them counts the rolled generation on this platform. The appender
    /// sorts by creation time where the filesystem has one and by the date in
    /// the name where it does not, and a rolled name has no parseable date —
    /// so the count, not the naming, is the thing to assert.
    #[test]
    fn a_roll_never_lifts_the_file_count_above_the_cap_plus_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}")), "old\n")
                .expect("seed old log");
        }
        std::fs::write(dir.path().join("carrick.log.2026-09-01.1"), "older still\n")
            .expect("seed rolled");
        // The appender's clock, read once and used for both the seed and the
        // roll, so the two cannot disagree across a midnight (carrick#1052).
        let day = today();
        assert_eq!(day, chrono::Utc::now().format("%Y-%m-%d").to_string());
        std::fs::write(
            dir.path().join(format!("carrick.log.{day}")),
            vec![b'x'; 512],
        )
        .expect("seed today");

        assert_eq!(roll_over_cap(dir.path(), &day, 256), Some(512));

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        let kept = log_files(dir.path());
        assert!(
            kept.len() <= RETAINED_LOG_DAYS + 1,
            "kept {} files, bound is {}: {kept:?}",
            kept.len(),
            RETAINED_LOG_DAYS + 1
        );
        assert!(
            kept.contains(&format!("carrick.log.{day}")),
            "today's file is not there: {kept:?}"
        );
        assert!(
            kept.iter()
                .filter(|name| name.ends_with(ROLLED_SUFFIX))
                .count()
                <= 1,
            "more than one rolled generation: {kept:?}"
        );
    }

    /// One clock, and it is the appender's (carrick#1052).
    ///
    /// `tracing_appender` names its files from `Utc::now()`. Everything that
    /// reads that name — the roll at startup, the offset the run-log upload
    /// slices from, `get_log_file_path` — goes through `today()`, so this is
    /// the assertion that keeps them on the same day. With `Local` here, every
    /// machine west of UTC had a window between local midnight and 00:00 UTC
    /// in which the upload looked for a file nobody was writing.
    #[test]
    fn todays_name_is_computed_on_the_appenders_clock() {
        assert_eq!(today(), chrono::Utc::now().format("%Y-%m-%d").to_string());

        // And the appender agrees, which is the half a constant cannot state.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");
        assert!(
            dir.path()
                .join(format!("carrick.log.{}", today()))
                .is_file(),
            "the appender wrote {:?}, not carrick.log.{}",
            log_files(dir.path()),
            today()
        );
    }

    /// The file layer keeps our own debug lines and drops everyone else's.
    ///
    /// The filter is the whole reason a laptop's log can be shipped: at a bare
    /// `debug` the file carried reqwest's and hyper's per-connection narration,
    /// which is where URLs, headers and pool internals appear. Driven through a
    /// real subscriber rather than by reading the directive string, so a filter
    /// that stops meaning what it says fails here.
    #[test]
    fn the_file_layer_keeps_our_debug_and_nobody_elses() {
        let written = Shared::default();
        let layer = fmt::layer()
            .with_writer(written.clone())
            .with_ansi(false)
            .with_target(true)
            .with_filter(EnvFilter::new(FILE_FILTER));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "carrick::engine", "ours at debug");
            tracing::debug!(target: "reqwest::connect", "theirs at debug");
            tracing::debug!(target: "hyper_util::client::legacy::pool", "theirs too");
            // Another crate's info is still worth keeping: it is rare, and a
            // warning from a dependency is often the only record of why a run
            // behaved the way it did.
            tracing::info!(target: "reqwest::connect", "theirs at info");
        });

        let log = written.text();
        assert!(log.contains("ours at debug"), "{log}");
        assert!(log.contains("theirs at info"), "{log}");
        assert!(!log.contains("theirs at debug"), "{log}");
        assert!(!log.contains("theirs too"), "{log}");
    }

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Shared {
        type Writer = Shared;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// A laptop's paths name the account and often the employer. The repo and
    /// the scanner version identify the run; the home directory identifies the
    /// person.
    #[test]
    fn the_home_directory_becomes_a_tilde() {
        assert_eq!(
            redact_log_line(
                "  reading /Users/ada/work/payments-api/src/index.ts",
                Some("/Users/ada")
            )
            .as_deref(),
            Some("  reading ~/work/payments-api/src/index.ts")
        );
        // Several times on one line, and in the middle of a longer word-shaped
        // path, because that is how a log writes them.
        assert_eq!(
            redact_home_in("/home/ada/a -> /home/ada/b", Some("/home/ada")),
            "~/a -> ~/b"
        );
        // Nothing to redact against is not an error; the line ships as it is.
        assert_eq!(redact_home_in("/home/ada/a", None), "/home/ada/a");
    }

    /// A line naming a credential does not leave the machine, whichever of the
    /// three shapes it names it in. None of these are logged today — the rule
    /// is what keeps a line added later from shipping one.
    #[test]
    fn a_line_naming_a_credential_is_dropped_whole() {
        for line in [
            "  DEBUG request headers: Authorization: Bearer abc",
            "  DEBUG presigned PUT https://b.s3/k?X-Amz-Signature=deadbeef",
            "  DEBUG credential carrick_sk_live_9f2 loaded",
        ] {
            assert_eq!(
                redact_log_line(line, Some("/home/ada")),
                None,
                "not dropped: {line}"
            );
        }
    }

    /// The two rules over a slice: the credential line goes, the rest keeps
    /// its order and loses its home directory.
    #[test]
    fn redacting_a_slice_drops_lines_and_rewrites_the_rest() {
        let redacted = redact_log("first line\n  Authorization: Bearer abc\nlast line\n");
        assert_eq!(redacted, "first line\nlast line\n");
    }

    fn log_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
