use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::info;
use tracing_appender::rolling;
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

/// Stable identifier for this scanner run. Initializes lazily on first
/// call to a fresh UUID v4, then returns the same string for the rest
/// of the process.
pub fn run_id() -> &'static str {
    RUN_ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Initialize the global tracing subscriber with two layers:
///
/// 1. **Terminal layer** (stderr): Shows `INFO` by default, `DEBUG` with `--verbose`.
///    Uses a minimal format without timestamps or targets for a clean look.
///
/// 2. **File layer** (best effort): when `~/.carrick/logs/` is writable, appends
///    `DEBUG`-level logs with timestamps to `carrick.log.YYYY-MM-DD` (daily
///    rotation). If the directory can't be created the file layer is skipped
///    and only the terminal layer is active — in that case the run preamble
///    only reaches stderr.
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

    if let Some(ref dir) = log_dir
        && std::fs::create_dir_all(dir).is_ok()
    {
        // Capture the current size of today's log file *before* we write
        // anything. Anything past this offset belongs to this run.
        let _ = RUN_START_OFFSET.set(current_log_file_size());

        let file_appender = rolling::daily(dir, "carrick.log");
        let file_layer = fmt::layer()
            .with_writer(file_appender)
            .with_ansi(false)
            .with_target(true)
            .with_filter(EnvFilter::new("debug"));

        let _ = tracing_subscriber::registry()
            .with(terminal_layer)
            .with(file_layer)
            .try_init();
        emit_run_preamble();
        return;
    }

    // Fallback: terminal only
    let _ = tracing_subscriber::registry()
        .with(terminal_layer)
        .try_init();
    emit_run_preamble();
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
    let log_dir = dirs::home_dir()?.join(".carrick").join("logs");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let log_file = log_dir.join(format!("carrick.log.{}", today));
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
