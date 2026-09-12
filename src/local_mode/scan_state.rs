//! A scan that outlives the shell that started it (carrick#992).
//!
//! `carrick index --infer` is the paid pass, and on a mid-sized monorepo it
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
//! Two rules the shape enforces:
//!
//! * **The file is evidence of a process, not of a result.** It exists while a
//!   scan runs, is removed when one finishes, and is left behind with
//!   `status: failed` when one fails. A file whose pid is gone is a scan that
//!   was killed — which is the case this ticket exists for, and the one state
//!   a reader most needs named.
//! * **Nothing here is on the path of a scan that is not detached.** Every
//!   writer is a no-op until [`begin`] has been called, which happens only in
//!   a child that was handed [`SCAN_ID_ENV`].
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

/// Set on the detached child, so it knows to keep this file and which id to
/// keep it under. Internal, like [`crate::progress::PROGRESS_ENV`].
pub const SCAN_ID_ENV: &str = "CARRICK_SCAN_ID";

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
    last_write: Option<Instant>,
    last_log: Option<Instant>,
}

/// What a detached scan is doing, as `carrick status` reads it back.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ScanState {
    pub scan_id: String,
    pub pid: u32,
    /// RFC 3339. `status` states the elapsed time from it rather than storing
    /// a duration that is wrong the moment it is written.
    pub started_at: String,
    pub updated_at: String,
    /// Whether this is the paid pass. A reader that sees a scan running wants
    /// to know whether it is spending money.
    pub infer: bool,
    pub workspace: String,
    pub status: ScanStatus,
    /// What the build is doing: `indexing <repo>`, `joining the workspace`.
    pub phase: String,
    /// The last count the scan reported inside that phase, if it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Update>,
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
    Failed,
}

impl ScanState {
    /// Seconds since this scan started, from the timestamp it recorded.
    pub fn elapsed_secs(&self) -> i64 {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&self.started_at) else {
            return 0;
        };
        (chrono::Utc::now() - started.with_timezone(&chrono::Utc))
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
    pub fn line(&self) -> String {
        let elapsed = human_duration(self.elapsed_secs());
        let counts = match &self.progress {
            Some(update) => format!(" — {}", update.render()),
            None => String::new(),
        };
        let paid = if self.infer { ", paid" } else { "" };
        if self.is_running() {
            return format!(
                "scan {} running for {elapsed}{paid}: {}{counts}",
                self.scan_id, self.phase
            );
        }
        match &self.error {
            Some(error) => format!("scan {} failed after {elapsed}: {error}", self.scan_id),
            // What a killed run left behind depends on how far it got: a
            // multi-repo build uploads each repo as it finishes it, and every
            // upload that came back with a figure was paid for. Saying
            // "nothing was uploaded" over the top of that would be false.
            None => match self.spend.as_ref().map(|spend| spend.scans.len()) {
                Some(paid) if paid > 0 => format!(
                    "scan {} stopped without finishing after {elapsed}, in {}{counts}. It had \
                     uploaded {paid} repo(s) and paid for them; run the command again.",
                    self.scan_id, self.phase
                ),
                _ => format!(
                    "scan {} stopped without finishing after {elapsed}, in {}{counts}. Nothing \
                     was uploaded by it; run the command again.",
                    self.scan_id, self.phase
                ),
            },
        }
    }
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

/// Start recording. Called once, in the child a `--detach` parent started.
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
        phase: "starting".to_string(),
        progress: None,
        error: None,
        spend: None,
    };
    let file = state_file(index_dir, scan_id);
    write(&file, &state);
    if let Ok(mut active) = ACTIVE.lock() {
        *active = Some(Active {
            file,
            state,
            last_write: None,
            last_log: None,
        });
    }
}

/// Say what the build is doing now. A no-op in a scan nobody detached.
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
    // change, and a pulse inside a long one.
    if phase_changed || due(active.last_log, LOG_GAP) {
        match &active.state.progress {
            Some(update) => eprintln!("carrick: {phase}: {}", update.render()),
            None => eprintln!("carrick: {phase}"),
        }
        active.last_log = Some(now);
    }
}

/// Record what this scan has paid so far. A no-op in a scan nobody detached,
/// like every other writer here.
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

/// The scan is over. Success removes the file; a failure leaves it with the
/// reason, so `carrick status` can say what happened rather than going quiet.
pub fn finish(error: Option<&str>) {
    let Ok(mut guard) = ACTIVE.lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };
    match error {
        None => {
            let _ = std::fs::remove_file(&active.file);
        }
        Some(error) => {
            active.state.status = ScanStatus::Failed;
            active.state.error = Some(error.to_string());
            active.state.updated_at = timestamp();
            write(&active.file, &active.state);
        }
    }
    *guard = None;
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

    /// A failure keeps its reason, because the log is long and the reason is
    /// one line of it.
    #[test]
    fn a_failed_scan_states_why() {
        let mut failed = state(ScanStatus::Failed, std::process::id());
        failed.error = Some("Carrick Cloud did not open this scan".to_string());
        let line = failed.line();
        assert!(line.contains("failed after 3m12s"), "{line}");
        assert!(line.contains("did not open this scan"), "{line}");
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

    #[test]
    fn durations_read_the_way_a_person_says_them() {
        assert_eq!(human_duration(41), "41s");
        assert_eq!(human_duration(192), "3m12s");
        assert_eq!(human_duration(3900), "1h05m");
    }
}
