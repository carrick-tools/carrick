//! Progress from a scan to whatever is waiting on it (carrick#955).
//!
//! `carrick index` runs one scan per repo as a subprocess and captures its
//! output, because a scan writes a report that means nothing to the indexer.
//! That left a first run silent for minutes at a time with nothing to
//! attribute the wait to, which is the same complaint that put one log line
//! per service in the engine (carrick#748) — except the indexer swallows those
//! too.
//!
//! So a scan states its progress on stderr in a line a parent can read:
//! `@carrick-progress {json}`, one object per update. It is written only when
//! the parent asks for it by setting [`PROGRESS_ENV`], so a CI log is
//! unchanged, and it goes to stderr beside the log lines rather than to stdout,
//! which carries the report.
//!
//! Updates are throttled to one every [`MIN_GAP`], with the last update of a
//! phase always sent: a file loop that ticks thousands of times must not turn
//! into thousands of lines, and a phase that ends must not leave a stale count
//! on the screen.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Set by a parent that reads the updates; unset everywhere else.
pub const PROGRESS_ENV: &str = "CARRICK_PROGRESS";

/// The line prefix. Chosen to be something no log line starts with.
const MARKER: &str = "@carrick-progress ";

const MIN_GAP: Duration = Duration::from_millis(200);

/// Which part of a service's scan an update is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Reading the service's files: the long phase of a local index.
    Files,
    /// Describing its functions: the long phase of a hosted scan.
    Intents,
}

/// One update, as it crosses the process boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub service: String,
    pub service_index: usize,
    pub service_total: usize,
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
}

impl Update {
    /// The one line a terminal shows for this update.
    pub fn render(&self) -> String {
        let unit = match self.phase {
            Phase::Files => "files",
            Phase::Intents => "intents",
        };
        // A repo with one service is already named by whoever is rendering
        // this, and its engine-side label for an unconfigured root is
        // "(root)", which says nothing twice.
        if self.service_total <= 1 {
            return format!("{} of {} {unit}", self.done, self.total);
        }
        format!(
            "{} ({}/{}): {} of {} {unit}",
            self.service, self.service_index, self.service_total, self.done, self.total
        )
    }
}

/// The service a scan is inside, set once per service by the engine.
static SERVICE: Mutex<Option<(String, usize, usize)>> = Mutex::new(None);
/// Milliseconds since the epoch at the last emitted update.
static LAST_EMIT: AtomicU64 = AtomicU64::new(0);

fn enabled() -> bool {
    std::env::var_os(PROGRESS_ENV).is_some()
}

/// Whether a parent process is reading this scan's stderr for markers.
///
/// One flag for the whole channel: the indexer sets it on every scan it
/// starts, and everything a scan states across that boundary — its progress,
/// and what it spent (carrick#995) — crosses only when someone asked for it.
pub fn parent_is_reading() -> bool {
    enabled()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// Name the service the following updates are about, and reset the throttle.
pub fn service_started(label: &str, index: usize, total: usize) {
    if !enabled() {
        return;
    }
    if let Ok(mut service) = SERVICE.lock() {
        *service = Some((label.to_string(), index, total));
    }
    LAST_EMIT.store(0, Ordering::Relaxed);
}

/// A tick within the current service. `done == total` always gets through.
pub fn tick(phase: Phase, done: usize, total: usize) {
    if !enabled() || total == 0 {
        return;
    }
    let final_tick = done >= total;
    let now = now_ms();
    if !final_tick {
        let last = LAST_EMIT.load(Ordering::Relaxed);
        if now.saturating_sub(last) < MIN_GAP.as_millis() as u64 {
            return;
        }
    }
    LAST_EMIT.store(now, Ordering::Relaxed);
    let Ok(service) = SERVICE.lock() else {
        return;
    };
    let (label, index, count) = service
        .clone()
        .unwrap_or_else(|| ("(scanning)".to_string(), 1, 1));
    drop(service);
    let update = Update {
        service: label,
        service_index: index,
        service_total: count,
        phase,
        done,
        total,
    };
    if let Ok(line) = serde_json::to_string(&update) {
        eprintln!("{MARKER}{line}");
    }
}

/// The line prefix for a scan's closing statement of the work it still owes.
const PENDING_MARKER: &str = "@carrick-pending ";

/// State, for the parent, which services this scan left pending and what
/// re-running does (the engine's summary line, verbatim).
///
/// The indexer swallows a scan's output and shows it only when the scan
/// fails, and a scan that lands six services and defers a seventh does not
/// fail — so without this the one sentence the user needs never reaches them.
pub fn report_pending(summary: &str) {
    if !enabled() {
        return;
    }
    eprintln!("{PENDING_MARKER}{}", summary.replace('\n', " "));
}

/// Read a pending statement out of a line of a scan's stderr.
pub fn parse_pending(line: &str) -> Option<String> {
    line.trim_start()
        .strip_prefix(PENDING_MARKER)
        .map(str::to_string)
}

/// Read one update out of a line of a scan's stderr, if that is what it is.
pub fn parse(line: &str) -> Option<Update> {
    let payload = line.trim_start().strip_prefix(MARKER)?;
    serde_json::from_str(payload).ok()
}

/// The prefix of the line a scan states a notice on.
const NOTICE_MARKER: &str = "@carrick-notice ";

#[derive(Serialize, Deserialize)]
struct Notice {
    text: String,
}

/// Say something a person watching the scan should know about why it is
/// slow: the model refusing work, the gateway throttling, requests being
/// retried.
///
/// Logged as an `info` line, which a direct scan's terminal shows. A scan the
/// indexer runs is a child whose plain stderr nobody sees until it fails, so
/// for a parent that is reading the same text also crosses as a marker, and
/// the indexer puts it beside the progress it shows (carrick#1122).
pub fn announce(text: &str) {
    tracing::info!("{text}");
    if !enabled() {
        return;
    }
    if let Ok(line) = serde_json::to_string(&Notice {
        text: text.to_string(),
    }) {
        eprintln!("{NOTICE_MARKER}{line}");
    }
}

/// Read a notice out of a line of a scan's stderr, if that is what it is.
pub fn parse_notice(line: &str) -> Option<String> {
    let payload = line.trim_start().strip_prefix(NOTICE_MARKER)?;
    serde_json::from_str::<Notice>(payload)
        .ok()
        .map(|notice| notice.text)
}

/// The prefix of the line a failing scan states its reason on.
const FAILURE_MARKER: &str = "@carrick-failure ";

/// Why a scan stopped, as it crosses to the process that started it.
///
/// A parent keeps a failed scan's stderr to show it, and that output is the
/// scanner's own log: parser noise, retry lines, internal names. What
/// `carrick status` prints for a failed scan is one sentence, and the only
/// process that knows that sentence is the one that failed, so it says it
/// here rather than leaving a parent to guess it from the log (carrick#1103).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// The stage the scan was in (`crate::scan_stage`'s token).
    pub stage: String,
    /// The error's first line, which is the sentence it leads with.
    pub reason: String,
}

/// State why this scan failed, for a parent that is reading. Nothing is
/// printed for anyone else: the error line the scan logs is theirs.
pub fn failed(stage: &str, error: &str) {
    if !enabled() {
        return;
    }
    if let Ok(line) = serde_json::to_string(&failure(stage, error)) {
        eprintln!("{FAILURE_MARKER}{line}");
    }
}

fn failure(stage: &str, error: &str) -> Failure {
    Failure {
        stage: stage.to_string(),
        reason: error
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("the scan stopped without an error message")
            .to_string(),
    }
}

/// Read a failure out of a line of a scan's stderr, if that is what it is.
pub fn parse_failure(line: &str) -> Option<Failure> {
    let payload = line.trim_start().strip_prefix(FAILURE_MARKER)?;
    serde_json::from_str(payload).ok()
}

/// A throttle for a caller that counts its own work, so a loop can tick on
/// every item and pay for it only when an update is actually due.
pub struct Ticker {
    phase: Phase,
    total: usize,
    done: usize,
    last: Instant,
}

impl Ticker {
    pub fn new(phase: Phase, total: usize) -> Self {
        Self {
            phase,
            total,
            done: 0,
            last: Instant::now(),
        }
    }

    /// Count one item, and send an update when one is due.
    pub fn item(&mut self) {
        self.done += 1;
        if self.done >= self.total || self.last.elapsed() >= MIN_GAP {
            self.last = Instant::now();
            tick(self.phase, self.done, self.total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_is_not_an_update_is_not_read_as_one() {
        assert!(parse("Analyzing service api (1/3)").is_none());
        assert!(parse("@carrick-progress not json").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn an_update_survives_the_round_trip() {
        let update = Update {
            service: "api".to_string(),
            service_index: 1,
            service_total: 3,
            phase: Phase::Files,
            done: 12,
            total: 40,
        };
        let line = format!("{MARKER}{}", serde_json::to_string(&update).unwrap());
        assert_eq!(parse(&line).as_ref(), Some(&update));
        assert_eq!(update.render(), "api (1/3): 12 of 40 files");
    }

    #[test]
    fn a_notice_survives_the_round_trip_and_is_not_another_marker() {
        let line = format!(
            "{NOTICE_MARKER}{}",
            serde_json::to_string(&Notice {
                text: "model busy: slowing analyze-file to 4 requests at a time".to_string()
            })
            .unwrap()
        );
        assert_eq!(
            parse_notice(&line).as_deref(),
            Some("model busy: slowing analyze-file to 4 requests at a time")
        );
        assert!(parse_notice("@carrick-progress {}").is_none());
        assert!(parse(&line).is_none());
        assert!(parse_failure(&line).is_none());
    }

    /// The reason is the error's leading sentence, and it survives the
    /// process boundary; a progress line is not read as a failure.
    #[test]
    fn a_failure_carries_the_errors_first_line() {
        let stated = failure(
            "discovery",
            "\n  A scan of acme/api is already running. (laptop_scan_in_flight, HTTP 409)\nbody: {}",
        );
        assert_eq!(
            stated.reason,
            "A scan of acme/api is already running. (laptop_scan_in_flight, HTTP 409)"
        );
        let line = format!(
            "{FAILURE_MARKER}{}",
            serde_json::to_string(&stated).unwrap()
        );
        assert_eq!(parse_failure(&line), Some(stated));
        assert!(parse_failure("@carrick-progress {}").is_none());
        assert_eq!(
            failure("upload", "").reason,
            "the scan stopped without an error message"
        );
    }

    #[test]
    fn a_single_service_is_not_numbered() {
        let update = Update {
            service: "repo".to_string(),
            service_index: 1,
            service_total: 1,
            phase: Phase::Intents,
            done: 3,
            total: 3,
        };
        assert_eq!(update.render(), "3 of 3 intents");
    }
}
