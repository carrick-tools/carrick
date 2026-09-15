//! A scan that outlives the shell that started it (carrick#992).
//!
//! `carrick index` is the paid pass, and on a mid-sized monorepo it
//! takes about fifteen minutes. The flow has an agent run it, and an agent's
//! shell caps a command at two minutes by default and ten at most: the scan is
//! killed part-way, the cloud's in-flight slot for that repo stays held until
//! its TTL, and the user — who never saw the spinner, because an agent's shell
//! is not a terminal — is told nothing.
//!
//! So the scan can be started detached. The parent returns immediately with a
//! scan id, the work runs in its own session with its output in
//! `.carrick/scan-<id>.log`, and this module is the small file beside it that
//! says what the scan is doing now: one JSON object, rewritten as the phases
//! and counts move, read back by `carrick status`.
//!
//! A scan run in the foreground keeps the same file. A user who starts
//! `carrick index` in one terminal and asks `carrick status` in another was
//! told there was no index and to run the command that was already running
//! (carrick#1132); the file is what lets the second terminal see the first.
//!
//! Two rules the shape enforces:
//!
//! * **The file is evidence of a process, and it outlives it by one build.** It
//!   exists while a scan runs and is rewritten when one ends: `finished` with
//!   the time it took, or `failed` with the reason — an interrupted one
//!   included. A file whose pid is gone while it still says `running` is a
//!   scan that was killed without a chance to say so, and the one state a
//!   reader most needs named. A finished record is cleared by the next build,
//!   not by the scan that wrote it: the scaffold tells an agent to poll
//!   `carrick status` until it says finished, and a record removed at the end
//!   of the scan means that sentence is never printed (carrick#1007 item 4).
//! * **Nothing here is on the path of a build that pays for nothing.** Every
//!   writer is a no-op until [`begin`] has been called, which only `carrick
//!   index` does; `refresh`, the session-start hook's command, leaves no
//!   record.
//!
//! What a run PAID is recorded here too, because a detached scan's output goes
//! to its log and a killed one leaves nothing else behind (carrick#995). It is
//! not the durable record: a scan that finishes removes this file, and
//! `.carrick/last-scan.json` is what `carrick status` repeats afterwards.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::progress::Update;

/// How often the state file is rewritten while a phase ticks. The updates
/// themselves are already throttled to five a second; a scan that runs for
/// fifteen minutes should not write four thousand files in that time.
const WRITE_GAP: Duration = Duration::from_secs(2);

/// How often a progress line is appended to the log. The log is what a user
/// tails, and a line every two seconds is a wall of text for a fifteen-minute
/// scan; a line every fifteen is a pulse.
const LOG_GAP: Duration = Duration::from_secs(15);

/// Where this process writes its state, once [`begin`] has been called.
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

struct Active {
    file: PathBuf,
    state: ScanState,
    started: Instant,
    last_write: Option<Instant>,
    last_log: Option<Instant>,
}

/// What a scan is doing, as `carrick status` reads it back.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ScanState {
    pub scan_id: String,
    pub pid: u32,
    /// RFC 3339. `status` states the elapsed time from it rather than storing
    /// a duration that is wrong the moment it is written.
    pub started_at: String,
    pub updated_at: String,
    /// Whether this scan was the paid pass. A reader that sees a scan running
    /// wants to know whether it is spending money.
    ///
    /// A record of what this run DID, not of a mode it could have been run
    /// without: `--infer` is gone and `carrick index` is the inferred scan
    /// itself (carrick#1008), and `index` is the only command that records a
    /// scan, so every record written today says true. It stays because the file is a
    /// receipt — the one thing a killed scan leaves behind — and a receipt
    /// that omits whether money was involved is not one (carrick#1023 item 7).
    pub infer: bool,
    pub workspace: String,
    pub status: ScanStatus,
    /// RFC 3339, when the scan ended. Set on a finished or failed record, so
    /// the line says how long the scan took rather than how long ago it
    /// started — those are the same number only while it is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// What the build is doing: `indexing <repo>`, `joining the workspace`.
    pub phase: String,
    /// The last count the scan reported inside that phase, if it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Update>,
    /// Why the scan is slow right now, in the words the scan used: the model
    /// refusing work, the gateway throttling, requests being retried
    /// (carrick#1122). Cleared by the next phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What this scan has paid Carrick Cloud so far, once a repo's upload has
    /// come back with a figure (carrick#995). A detached scan's own output
    /// goes to its log, so this file is where a reader of a run nobody watched
    /// finds what it cost — a killed one included, because the money was spent
    /// at the upload. Absent on the free pass and before the first upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<crate::scan_spend::RunSpend>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Running,
    Finished,
    Failed,
}

impl ScanState {
    /// How long this scan has been going, or how long it took: seconds from
    /// the timestamp it started at to the one it ended at, and to now while it
    /// is still running.
    pub fn elapsed_secs(&self) -> i64 {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&self.started_at) else {
            return 0;
        };
        let until = self
            .finished_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        (until - started.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0)
    }

    /// Whether the process that wrote this file is still there.
    ///
    /// `kill(pid, 0)` asks the kernel about a pid without touching it: `Ok`
    /// means it exists, `EPERM` means it exists and belongs to someone else,
    /// and only `ESRCH` means it is gone. A pid can be reused, which would
    /// make a dead scan read as alive; the state file is removed when a scan
    /// ends, so the window for that is a scan that was killed AND a pid that
    /// wrapped since.
    ///
    /// The conversion is checked rather than cast: `kill` reads a NEGATIVE
    /// argument as a process group, so a recorded pid that does not fit would
    /// stop being a question about a process at all.
    #[cfg(unix)]
    pub fn is_running(&self) -> bool {
        if self.status != ScanStatus::Running {
            return false;
        }
        let Ok(pid) = libc::pid_t::try_from(self.pid) else {
            return false;
        };
        // SAFETY: `kill` with signal 0 performs no action; it only reports
        // whether the pid can be signalled.
        let answered = unsafe { libc::kill(pid, 0) };
        answered == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(not(unix))]
    pub fn is_running(&self) -> bool {
        // Without a cheap liveness probe, the recorded status is the answer:
        // a scan that finished removed this file, and one that failed rewrote
        // it. A killed scan reads as running on this platform.
        self.status == ScanStatus::Running
    }

    /// The one line `carrick status` prints for this scan.
    ///
    /// Each line opens `scan <id> <running|finished|failed|stopped>`, which is
    /// what the scaffold tells an agent polling a detached scan to read, and
    /// then says when it started, how long it ran and, for a failure, the one
    /// sentence of why. A failure's `error` also carries an excerpt of the
    /// scan's own log; that is for `--json` and for whoever opens the log, and
    /// never printed here: its lines name internals a user cannot act on
    /// (carrick#1103).
    pub fn line(&self) -> String {
        let elapsed = human_duration(self.elapsed_secs());
        let started = self.started_label();
        let counts = match &self.progress {
            Some(update) => format!(" — {}", update.render()),
            None => String::new(),
        };
        let paid = if self.infer { ", paid" } else { "" };
        if self.is_running() {
            let notice = match &self.notice {
                Some(notice) => format!(" ({notice})"),
                None => String::new(),
            };
            return format!(
                "scan {} running for {elapsed} (started {started}{paid}): {}{counts}{notice}",
                self.scan_id, self.phase
            );
        }
        // The word the scaffold's poll loop waits for. It is stated by the
        // record the finished scan leaves behind, and the next build clears it
        // (carrick#1007 item 4).
        if self.status == ScanStatus::Finished {
            return format!(
                "scan {} finished after {elapsed} (started {started}{paid}). The index is written.",
                self.scan_id
            );
        }
        match self.reason() {
            Some(reason) => format!(
                "scan {} failed after {elapsed} (started {started}): {reason}",
                self.scan_id
            ),
            // What a killed run left behind depends on how far it got: a
            // multi-repo build uploads each repo as it finishes it, and every
            // upload that came back with a figure was paid for. Saying
            // "nothing was uploaded" over the top of that would be false.
            None => match self.spend.as_ref().map(|spend| spend.scans.len()) {
                Some(paid) if paid > 0 => format!(
                    "scan {} stopped without finishing after {elapsed} (started {started}), in \
                     {}{counts}. It had uploaded {paid} repo(s) and paid for them; run the \
                     command again.",
                    self.scan_id, self.phase
                ),
                _ => format!(
                    "scan {} stopped without finishing after {elapsed} (started {started}), in \
                     {}{counts}. Nothing was uploaded by it; run the command again.",
                    self.scan_id, self.phase
                ),
            },
        }
    }

    /// Why a failed scan failed, in the sentence its error leads with.
    ///
    /// Every writer of `error` puts that sentence first: a scan subprocess
    /// states it on the progress channel and the build leads with it
    /// (`index::run_scan`), and a build that fails before any scan has a
    /// one-line error to begin with. What follows the first line is the log
    /// excerpt.
    pub fn reason(&self) -> Option<&str> {
        let error = self.error.as_deref()?;
        Some(
            error
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("no reason was recorded"),
        )
    }

    /// When the scan started, on this machine's clock: the time alone for a
    /// scan started today, the date with it otherwise.
    fn started_label(&self) -> String {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&self.started_at) else {
            return self.started_at.clone();
        };
        let local = started.with_timezone(&chrono::Local);
        if local.date_naive() == chrono::Local::now().date_naive() {
            local.format("%H:%M").to_string()
        } else {
            local.format("%Y-%m-%d %H:%M").to_string()
        }
    }

    /// A scan that failed within a minute of starting: a refusal, which a
    /// re-run repeats word for word.
    fn failed_at_once(&self) -> bool {
        !self.is_running() && self.status == ScanStatus::Failed && self.elapsed_secs() < 60
    }
}

/// What `carrick status` prints for the scans a workspace has records of.
///
/// One line per scan, the running scan first and then newest first. A run of
/// scans that each failed within a minute with the same reason is one line
/// naming every id: three re-runs refused because a scan was still in flight
/// are one fact, and each printed whole was a screen of the same sentence
/// (carrick#1103). The log directory is named once, at the end, for the
/// reader who wants more than the sentence — and only when one of these scans
/// has a log there: a scan run in the foreground printed to its terminal, and
/// pointing at a file it never wrote sends the reader to nothing
/// (carrick#1132).
pub fn status_lines(scans: &[ScanState], index_dir: &Path) -> Vec<String> {
    if scans.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<&ScanState> = scans.iter().collect();
    // Stable, so scans of the same kind keep `read_all`'s newest-first order.
    ordered.sort_by_key(|scan| !scan.is_running());

    let mut lines = Vec::new();
    let mut index = 0;
    while index < ordered.len() {
        let scan = ordered[index];
        let mut same: Vec<&str> = Vec::new();
        if scan.failed_at_once() {
            while let Some(next) = ordered.get(index + 1 + same.len()) {
                if next.failed_at_once() && next.reason() == scan.reason() {
                    same.push(&next.scan_id);
                } else {
                    break;
                }
            }
        }
        let mut line = scan.line();
        if !same.is_empty() {
            line.push_str(&format!(
                " The same happened to {} earlier scan(s): {}.",
                same.len(),
                same.join(", ")
            ));
        }
        lines.push(line);
        index += 1 + same.len();
    }
    if scans
        .iter()
        .any(|scan| log_file(index_dir, &scan.scan_id).is_file())
    {
        lines.push(format!(
            "Each scan's full output is in {}.",
            log_file(index_dir, "<id>").display()
        ));
    }
    lines
}

/// `3m12s`, `41s`, `1h04m`.
fn human_duration(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `.carrick/scan-<id>.json`: what the scan is doing.
pub fn state_file(index_dir: &Path, scan_id: &str) -> PathBuf {
    index_dir.join(format!("scan-{scan_id}.json"))
}

/// `.carrick/scan-<id>.log`: everything the scan printed.
pub fn log_file(index_dir: &Path, scan_id: &str) -> PathBuf {
    index_dir.join(format!("scan-{scan_id}.log"))
}

/// The id a scan is recorded under: the head of this run's id.
///
/// The run id is already the key that joins a build's own logs to the cloud's,
/// and a detached child inherits it from the parent that printed the id, so
/// the scan id is not a second identifier for one run and needs no channel of
/// its own to reach the child.
pub fn scan_id() -> String {
    crate::logging::run_id().chars().take(8).collect()
}

/// Start recording. Called once, by `carrick index`, detached or not.
///
/// The directory is created here: a first index run in the foreground reaches
/// this before anything else has made `.carrick`, and a record that could not
/// be written is the silence carrick#1132 was about.
pub fn begin(index_dir: &Path, scan_id: &str, workspace: &Path, infer: bool) {
    let now = timestamp();
    let state = ScanState {
        scan_id: scan_id.to_string(),
        pid: std::process::id(),
        started_at: now.clone(),
        updated_at: now,
        infer,
        workspace: workspace.to_string_lossy().into_owned(),
        status: ScanStatus::Running,
        finished_at: None,
        phase: "starting".to_string(),
        progress: None,
        notice: None,
        error: None,
        spend: None,
    };
    let _ = std::fs::create_dir_all(index_dir);
    let file = state_file(index_dir, scan_id);
    write(&file, &state);
    if let Ok(mut active) = ACTIVE.lock() {
        *active = Some(Active {
            file,
            state,
            started: Instant::now(),
            last_write: None,
            last_log: None,
        });
    }
}

/// Say what the build is doing now. A no-op in a build that records no scan.
///
/// `update` is the count inside the phase, when the phase reports one. The
/// same call carries both, because a phase with no counts yet must still
/// replace the previous phase's.
pub fn note(phase: &str, update: Option<&Update>) {
    let Ok(mut guard) = ACTIVE.lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    let phase_changed = active.state.phase != phase;
    active.state.phase = phase.to_string();
    if let Some(update) = update {
        active.state.progress = Some(update.clone());
    } else if phase_changed {
        active.state.progress = None;
    }
    if phase_changed {
        active.state.notice = None;
    }

    let now = Instant::now();
    let due = |last: Option<Instant>, gap: Duration| {
        last.is_none_or(|last| now.duration_since(last) >= gap)
    };
    if phase_changed || due(active.last_write, WRITE_GAP) {
        active.state.updated_at = timestamp();
        write(&active.file, &active.state);
        active.last_write = Some(now);
    }
    // The log is the other half of the answer: `carrick status` says where a
    // scan is, and the log says where it has been. A line on every phase
    // change, on the last count of a phase, and a pulse inside a long one.
    //
    // The last count matters because without it a phase's line stops at
    // whatever number happened to be due — the log's final word on a service
    // was "1 of 5 intents" for a service that finished all five. Each line
    // carries the scan's age, so a reader tailing the log can tell a pulse
    // from a repeat (carrick#1007 item 3).
    let counted_out = active
        .state
        .progress
        .as_ref()
        .is_some_and(|update| update.total > 0 && update.done >= update.total);
    if phase_changed || counted_out || due(active.last_log, LOG_GAP) {
        let age = human_duration(now.duration_since(active.started).as_secs() as i64);
        match &active.state.progress {
            Some(update) => log_line(&format!("carrick: {age} {phase}: {}", update.render())),
            None => log_line(&format!("carrick: {age} {phase}")),
        }
        active.last_log = Some(now);
    }
}

/// Record why the scan is slow, as the scan said it (carrick#1122): into the
/// state `carrick status` reads, and as a line in the log, at once rather than
/// at the next pulse. A no-op in a build that records no scan. The next phase
/// clears it, because a busy model in one repo's scan says nothing about the next.
pub fn notice(text: &str) {
    let Ok(mut guard) = ACTIVE.lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    active.state.notice = Some(text.to_string());
    active.state.updated_at = timestamp();
    write(&active.file, &active.state);
    let now = Instant::now();
    active.last_write = Some(now);
    let age = human_duration(now.duration_since(active.started).as_secs() as i64);
    log_line(&format!("carrick: {age} {text}"));
}

/// A line for whoever reads this scan's output as text: a detached scan's log,
/// or the output an agent's shell captured from a foreground one.
///
/// Not a terminal. There the spinner is already saying the same thing, and a
/// line printed under it every fifteen seconds tears the bar it is drawing —
/// the same split `logging::spinner` makes for its own lines.
fn log_line(line: &str) {
    if !crate::logging::is_tty() {
        eprintln!("{line}");
    }
}

/// Record what this scan has paid so far. A no-op in a build that records no
/// scan, like every other writer here.
///
/// Written on the spot rather than at the end: a scan that is killed after an
/// upload has already spent the money, and this file is what it leaves behind.
pub fn spent(spend: &crate::scan_spend::RunSpend) {
    let Ok(mut guard) = ACTIVE.lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    active.state.spend = Some(spend.clone());
    active.state.updated_at = timestamp();
    write(&active.file, &active.state);
    active.last_write = Some(Instant::now());
}

/// The scan is over, and the record says which way and how long it took.
///
/// Neither outcome removes the file. The scaffold has an agent poll `carrick
/// status` until it says finished, stopped or failed; a record deleted here
/// made the first of those unsayable — the scan simply stopped being mentioned
/// and the poll never terminated on the word it was told to wait for
/// (carrick#1007 item 4). [`forget_finished`] clears it at the next build.
pub fn finish(error: Option<&str>) {
    let Ok(mut guard) = ACTIVE.lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    active.state.status = match error {
        None => ScanStatus::Finished,
        Some(error) => {
            active.state.error = Some(error.to_string());
            ScanStatus::Failed
        }
    };
    let now = timestamp();
    active.state.finished_at = Some(now.clone());
    active.state.updated_at = now;
    write(&active.file, &active.state);
    // The last line of the log, so a reader tailing it sees the end rather
    // than a progress count that simply stopped moving.
    log_line(&format!("carrick: {}", active.state.line()));
    *guard = None;
}

/// The scan was ended by a signal, and the record says so before the process
/// goes (carrick#1132).
///
/// Without it a Ctrl-C left `running` with a dead pid, which `carrick status`
/// can only call "stopped without finishing". The signal is the reason, and so
/// is what the scan had already paid for: a multi-repo build uploads each repo
/// as it finishes it, and the money for those is spent.
pub fn interrupted(signal: &str) {
    let reason = {
        let Ok(guard) = ACTIVE.lock() else {
            return;
        };
        let Some(active) = guard.as_ref() else {
            return;
        };
        interruption_reason(signal, active.state.spend.as_ref())
    };
    finish(Some(&reason));
}

/// The sentence an interrupted scan's record leads with.
fn interruption_reason(signal: &str, spend: Option<&crate::scan_spend::RunSpend>) -> String {
    match spend.map(|spend| spend.scans.len()) {
        Some(paid) if paid > 0 => format!(
            "interrupted by {signal}, after uploading {paid} repo(s) and paying for them; run \
             the command again."
        ),
        _ => {
            format!("interrupted by {signal} before anything was uploaded; run the command again.")
        }
    }
}

/// Drop the records of scans that ended well, at the start of a build.
///
/// A finished record is kept so that `carrick status` can say the word once,
/// and a build is the event that makes it history: whatever it says about the
/// index, this build is writing a newer one. A failed record is left alone —
/// its reason is the only trace of what went wrong — as is any scan still
/// running, including this build's own.
pub fn forget_finished(index_dir: &Path) {
    for state in read_all(index_dir) {
        if state.status == ScanStatus::Finished {
            let _ = std::fs::remove_file(state_file(index_dir, &state.scan_id));
        }
    }
}

/// Drop every record an index that has just been written makes history.
///
/// A failed scan's reason is the only trace of what went wrong, so it is kept
/// — until a later build succeeds, at which point it describes a world that no
/// longer exists and `carrick status` was still leading with it minutes after
/// a good index landed (carrick#1023 item 13). "Later" is the point: a record
/// is dropped whether the scan behind it was detached or not, and whether it
/// failed, finished or was killed.
///
/// A scan that is still RUNNING is never dropped, which is what keeps this
/// build's own record — written by [`begin`], rewritten by [`finish`] after
/// this runs — and any concurrent scan's.
pub fn forget_superseded(index_dir: &Path) {
    for state in read_all(index_dir) {
        if !state.is_running() {
            let _ = std::fs::remove_file(state_file(index_dir, &state.scan_id));
        }
    }
}

/// Every scan this workspace has a state file for, newest first.
pub fn read_all(index_dir: &Path) -> Vec<ScanState> {
    let Ok(entries) = std::fs::read_dir(index_dir) else {
        return Vec::new();
    };
    let mut states: Vec<ScanState> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("scan-") && name.ends_with(".json"))
        })
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<ScanState>(&text).ok())
        .collect();
    states.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    states
}

/// Write the state where a reader can never see half of it.
fn write(file: &Path, state: &ScanState) {
    let Ok(json) = serde_json::to_vec_pretty(state) else {
        return;
    };
    let pending = file.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&pending, json).is_ok() {
        let _ = std::fs::rename(&pending, file);
    }
    let _ = std::fs::remove_file(pending);
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Phase;

    fn state(status: ScanStatus, pid: u32) -> ScanState {
        ScanState {
            scan_id: "5089ed60".to_string(),
            pid,
            started_at: (chrono::Utc::now() - chrono::Duration::seconds(192)).to_rfc3339(),
            updated_at: timestamp(),
            infer: true,
            workspace: "/repos".to_string(),
            status,
            finished_at: None,
            phase: "indexing gateway".to_string(),
            spend: None,
            progress: Some(Update {
                service: "gateway".to_string(),
                service_index: 1,
                service_total: 2,
                phase: Phase::Files,
                done: 118,
                total: 240,
            }),
            error: None,
            notice: None,
        }
    }

    /// What a user asks a running scan: which service, how far, how long.
    #[test]
    fn a_running_scan_names_its_phase_its_counts_and_its_age() {
        let line = state(ScanStatus::Running, std::process::id()).line();
        assert!(line.contains("scan 5089ed60 running for 3m12s"), "{line}");
        assert!(line.contains("indexing gateway"), "{line}");
        assert!(line.contains("118 of 240 files"), "{line}");
        assert!(line.contains("paid"), "the paid pass says so: {line}");
    }

    /// A running scan the model is slowing says why, after its counts
    /// (carrick#1122).
    #[test]
    fn a_running_scan_says_why_it_is_slow() {
        let mut slowed = state(ScanStatus::Running, std::process::id());
        slowed.notice =
            Some("model busy: slowing analyze-file to 4 requests at a time".to_string());
        let line = slowed.line();
        assert!(
            line.ends_with(
                "118 of 240 files (model busy: slowing analyze-file to 4 requests at a time)"
            ),
            "{line}"
        );
    }

    /// The case the ticket exists for: an agent's shell killed the scan, and
    /// nothing else would ever say so.
    #[cfg(unix)]
    #[test]
    fn a_scan_whose_process_is_gone_says_it_stopped() {
        // A pid above every platform's `pid_max` (4194304 on Linux, 99998 on
        // macOS) and still positive: `kill` reads a negative argument as a
        // process GROUP, so a pid chosen by wrapping would ask a different
        // question and could be answered "yes" by an unrelated group.
        let mut killed = state(ScanStatus::Running, i32::MAX as u32);
        killed.progress = None;
        let line = killed.line();
        assert!(line.contains("stopped without finishing"), "{line}");
        assert!(line.contains("run the command again"), "{line}");
    }

    /// A failure states its reason, because the log is long and the reason is
    /// one line of it; the excerpt of the log that rides beside it in `error`
    /// is not printed (carrick#1103).
    #[test]
    fn a_failed_scan_states_why_and_nothing_of_its_log() {
        let mut failed = state(ScanStatus::Failed, std::process::id());
        failed.error = Some(
            "the scan of api failed: Carrick Cloud did not open this scan\n\
             Using TeeStorage (laptop scan: cloud upload + local cache)\n\
             ... 2756 line(s) not shown, except ..."
                .to_string(),
        );
        let line = failed.line();
        assert!(
            line.starts_with("scan 5089ed60 failed after 3m12s (started "),
            "{line}"
        );
        assert!(
            line.ends_with("the scan of api failed: Carrick Cloud did not open this scan"),
            "{line}"
        );
        assert!(!line.contains("TeeStorage"), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }

    /// A scan that failed within a minute of starting, with `reason` as the
    /// sentence and the scanner's own log after it.
    fn refused(id: &str, seconds_ago: i64, reason: &str) -> ScanState {
        let started = chrono::Utc::now() - chrono::Duration::seconds(seconds_ago);
        let mut scan = state(ScanStatus::Failed, std::process::id());
        scan.scan_id = id.to_string();
        scan.started_at = started.to_rfc3339();
        scan.finished_at = Some((started + chrono::Duration::seconds(1)).to_rfc3339());
        scan.progress = None;
        scan.error = Some(format!(
            "the scan of api failed: {reason}\n\
             Carrick run continuing run_id=4f2a\n\
             Initializing sidecar...\n\
             [FileOrchestrator] Type resolution warnings: [\"/Users/ada/api/src/a.ts\"]\n\
             Gateway status 429 with non-envelope body (missing field `success`)\n\
             ... 2756 line(s) not shown, except ..."
        ));
        scan
    }

    /// The report from the field: a running scan, three re-runs refused
    /// because it was running, and an older failure. One line each, the
    /// running scan on top, the refusals as one line naming every id, the
    /// log directory once, and none of the scanner's internals (carrick#1103).
    #[test]
    fn status_prints_one_line_per_scan_in_user_terms() {
        let busy = "A scan of acme/api from this workspace is already running. \
                    (laptop_scan_in_flight, HTTP 409)";
        let mut running = state(ScanStatus::Running, std::process::id());
        running.scan_id = "runs0001".to_string();
        running.started_at = (chrono::Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        let mut older = refused(
            "old00001",
            7200,
            "No JS/TS source files found under the repository root.",
        );
        older.finished_at =
            Some((chrono::Utc::now() - chrono::Duration::seconds(6000)).to_rfc3339());
        // Newest first, as `read_all` returns them.
        let scans = vec![
            refused("ref00003", 30, busy),
            refused("ref00002", 90, busy),
            refused("ref00001", 150, busy),
            running,
            older,
        ];

        // The running scan was detached, so its log is on disk.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            log_file(dir.path(), "runs0001"),
            "carrick: 10m00s indexing\n",
        )
        .unwrap();
        let lines = status_lines(&scans, dir.path());

        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(
            lines[0].starts_with("scan runs0001 running for 10m00s (started "),
            "{lines:#?}"
        );
        assert!(
            lines[1].starts_with("scan ref00003 failed after 1s (started "),
            "{lines:#?}"
        );
        assert!(lines[1].contains(busy), "{lines:#?}");
        assert!(
            lines[1].ends_with("The same happened to 2 earlier scan(s): ref00002, ref00001."),
            "{lines:#?}"
        );
        assert!(
            lines[2].starts_with("scan old00001 failed after "),
            "{lines:#?}"
        );
        assert!(
            lines[2].contains("No JS/TS source files found"),
            "{lines:#?}"
        );
        assert_eq!(
            lines[3],
            format!(
                "Each scan's full output is in {}.",
                dir.path().join("scan-<id>.log").display()
            )
        );
        for internal in [
            "TeeStorage",
            "sidecar",
            "FileOrchestrator",
            "non-envelope",
            "not shown",
            "run_id",
            "/Users/ada",
        ] {
            assert!(
                lines.iter().all(|line| !line.contains(internal)),
                "{internal} reached the terminal: {lines:#?}"
            );
        }
    }

    /// A long failure is not a refusal, so two of them with the same reason are
    /// still two lines; and a workspace with no records prints nothing at all.
    #[test]
    fn only_quick_failures_with_the_same_reason_collapse() {
        let mut slow = refused("slow0002", 900, "the model quota was exhausted");
        slow.finished_at = Some(chrono::Utc::now().to_rfc3339());
        let mut slower = refused("slow0001", 1900, "the model quota was exhausted");
        slower.finished_at =
            Some((chrono::Utc::now() - chrono::Duration::seconds(1000)).to_rfc3339());
        let lines = status_lines(
            &[slow, slower, refused("fast0001", 3000, "something else")],
            Path::new("/w/.carrick"),
        );
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(status_lines(&[], Path::new("/w/.carrick")).is_empty());
    }

    /// A scan run in the foreground printed to its own terminal and has no log
    /// in `.carrick`, so `status` names the scan and points at no file
    /// (carrick#1132).
    #[test]
    fn a_foreground_scan_is_named_without_a_log_it_never_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let running = state(ScanStatus::Running, std::process::id());
        let lines = status_lines(&[running], dir.path());
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(
            lines[0].starts_with("scan 5089ed60 running for "),
            "{lines:#?}"
        );
    }

    /// An interrupted scan says what ended it, and whether it had already
    /// paid for anything before it did (carrick#1132).
    #[test]
    fn an_interrupted_scan_names_the_signal_and_what_it_paid() {
        let nothing = interruption_reason("SIGINT", None);
        assert_eq!(
            nothing,
            "interrupted by SIGINT before anything was uploaded; run the command again."
        );
        let mut spend = crate::scan_spend::RunSpend::default();
        spend.record(
            "api",
            crate::scan_spend::ScanSpend {
                schema: crate::scan_spend::SCHEMA.to_string(),
                priced: true,
                usd: Some(4.32),
                ..Default::default()
            },
        );
        let paid = interruption_reason("SIGTERM", Some(&spend));
        assert_eq!(
            paid,
            "interrupted by SIGTERM, after uploading 1 repo(s) and paying for them; run the \
             command again."
        );
        let mut failed = state(ScanStatus::Failed, std::process::id());
        failed.error = Some(paid);
        assert!(
            failed.line().ends_with("interrupted by SIGTERM, after uploading 1 repo(s) and paying for them; run the command again."),
            "{}",
            failed.line()
        );
    }

    /// A detached run's output goes to its log, so what it paid is kept here
    /// as it is paid — and a scan that was killed after uploading a repo did
    /// spend that money. "Nothing was uploaded by it" would be false.
    #[test]
    #[cfg(unix)]
    fn a_killed_scan_that_had_already_paid_does_not_claim_it_uploaded_nothing() {
        let mut killed = state(ScanStatus::Running, i32::MAX as u32);
        let mut spend = crate::scan_spend::RunSpend::default();
        spend.record(
            "api",
            crate::scan_spend::ScanSpend {
                schema: crate::scan_spend::SCHEMA.to_string(),
                priced: true,
                usd: Some(4.32),
                ..Default::default()
            },
        );
        killed.spend = Some(spend);
        let line = killed.line();
        assert!(
            line.contains("uploaded 1 repo(s) and paid for them"),
            "{line}"
        );
        assert!(!line.contains("Nothing \\\nwas uploaded"), "{line}");
        // The figure itself is not on this line: `carrick status` prints the
        // receipt once, and a scan line that repeated it would say it twice.
        assert!(!line.contains("US$"), "{line}");
    }

    /// A state file written before the field existed is still a scan this
    /// binary can report on.
    #[test]
    fn a_state_file_without_a_spend_still_parses() {
        let stored = serde_json::json!({
            "scan_id": "5089ed60",
            "pid": 4321,
            "started_at": "2026-09-12T21:00:00Z",
            "updated_at": "2026-09-12T21:03:00Z",
            "infer": true,
            "workspace": "/repos",
            "status": "running",
            "phase": "indexing gateway"
        });
        let read: ScanState = serde_json::from_value(stored).unwrap();
        assert!(read.spend.is_none());
    }

    #[test]
    fn the_state_survives_the_round_trip_and_is_found_by_a_reader() {
        let dir = tempfile::tempdir().unwrap();
        let written = state(ScanStatus::Running, std::process::id());
        write(&state_file(dir.path(), &written.scan_id), &written);
        let read = read_all(dir.path());
        assert_eq!(read, vec![written]);
        assert!(read_all(&dir.path().join("nothing-here")).is_empty());
    }

    /// The scaffold tells an agent to poll until `status` says finished; the
    /// scan used to stop being mentioned instead, so the word never arrived
    /// (carrick#1007 item 4).
    #[test]
    fn a_finished_scan_says_so_with_the_time_it_took() {
        let mut finished = state(ScanStatus::Finished, std::process::id());
        finished.finished_at = Some(
            (chrono::DateTime::parse_from_rfc3339(&finished.started_at).unwrap()
                + chrono::Duration::seconds(58))
            .to_rfc3339(),
        );
        let line = finished.line();
        assert!(line.contains("scan 5089ed60 finished after 58s"), "{line}");
        assert!(line.contains("paid"), "{line}");
        assert!(line.contains("The index is written"), "{line}");
        // Not "stopped without finishing", which is what every non-running
        // record said before this state existed.
        assert!(!line.contains("stopped"), "{line}");
        assert!(!finished.is_running());
    }

    /// The record is kept for the poll to read and cleared by the next build.
    /// A failed one stays: its reason is the only trace of what went wrong.
    #[test]
    fn the_next_build_forgets_a_finished_scan_and_keeps_a_failed_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut finished = state(ScanStatus::Finished, std::process::id());
        finished.scan_id = "aaaaaaaa".to_string();
        let mut failed = state(ScanStatus::Failed, std::process::id());
        failed.scan_id = "bbbbbbbb".to_string();
        let running = state(ScanStatus::Running, std::process::id());
        for scan in [&finished, &failed, &running] {
            write(&state_file(dir.path(), &scan.scan_id), scan);
        }
        forget_finished(dir.path());
        let left: Vec<String> = read_all(dir.path())
            .into_iter()
            .map(|scan| scan.scan_id)
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(!left.contains(&"aaaaaaaa".to_string()), "{left:?}");
    }

    #[test]
    fn durations_read_the_way_a_person_says_them() {
        assert_eq!(human_duration(41), "41s");
        assert_eq!(human_duration(192), "3m12s");
        assert_eq!(human_duration(3900), "1h05m");
    }
}
