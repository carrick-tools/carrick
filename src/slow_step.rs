//! Names the file a slow per-file step is on (carrick#3766).
//!
//! Discovery reads every file in one synchronous loop. A customer scan hung
//! inside it for hours and the log said nothing after `Found N files`, so the
//! file could not be named. [`Watch`] runs a small thread beside the loop: the
//! loop says which file and step it is on, and once one step has run past the
//! threshold the thread reports it, then again every repeat interval while it
//! is still stuck. A normal scan never reaches the threshold and logs nothing.
//!
//! It only reports. It never stops, skips or times out a read (that is a
//! separate decision), and the loop pays one short lock per step.
//!
//! The line is logged at info, so it reaches the terminal and the log the
//! runner keeps even when the end-of-run log upload does not arrive. It
//! appears only after a step has run past the threshold, never on a healthy
//! scan. Paths are repo-relative, never the repo's own location. The watch
//! also covers the stages after the per-file loop (`stage`), which name no
//! file.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Seconds one step may run before it is reported. Override with
/// `CARRICK_SLOW_FILE_SECS` (a test hook, and a lever for a support run).
const DEFAULT_THRESHOLD: Duration = Duration::from_secs(30);
/// How often a still-stuck step is reported again.
const DEFAULT_REPEAT: Duration = Duration::from_secs(300);
const THRESHOLD_ENV: &str = "CARRICK_SLOW_FILE_SECS";

struct State {
    file: String,
    step: &'static str,
    since: Instant,
    next_report: Instant,
    idle: bool,
    done: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub struct Watch {
    shared: Arc<Shared>,
    threshold: Duration,
    thread: Option<JoinHandle<()>>,
}

/// The threshold in force: the env override when it parses, else the default.
pub fn threshold_from_env() -> Duration {
    std::env::var(THRESHOLD_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(DEFAULT_THRESHOLD)
}

impl Watch {
    /// Starts the watcher with the environment's threshold, logging at info.
    pub fn discovery() -> Self {
        Self::discovery_at(threshold_from_env(), DEFAULT_REPEAT)
    }

    /// The discovery watcher with explicit timings (a test's short ones).
    pub(crate) fn discovery_at(threshold: Duration, repeat: Duration) -> Self {
        // The watch thread logs through the dispatcher of the thread that
        // started it, so a scoped subscriber (a test's) sees the line too.
        let dispatch = tracing::Dispatch::default();
        Self::start(threshold, repeat, move |line| {
            tracing::dispatcher::with_default(&dispatch, || tracing::info!("{line}"))
        })
    }

    /// Starts a watcher that hands each report to `emit`.
    pub fn start(
        threshold: Duration,
        repeat: Duration,
        emit: impl Fn(String) + Send + 'static,
    ) -> Self {
        let now = Instant::now();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                file: String::new(),
                step: "",
                since: now,
                next_report: now,
                idle: true,
                done: false,
            }),
            wake: Condvar::new(),
        });
        // Check often enough to report close to the threshold, rarely enough
        // to cost nothing.
        let tick = (threshold / 4).clamp(Duration::from_millis(5), Duration::from_secs(1));
        let seen = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("slow-step-watch".to_string())
            .spawn(move || {
                let mut state = seen.state.lock().unwrap_or_else(|e| e.into_inner());
                while !state.done {
                    let now = Instant::now();
                    if !state.idle && now >= state.next_report {
                        let secs = now.duration_since(state.since).as_secs();
                        emit(if state.file.is_empty() {
                            format!("discovery: still in {} after {} s", state.step, secs)
                        } else {
                            format!(
                                "discovery: still reading {} (step {}) after {} s",
                                state.file, state.step, secs
                            )
                        });
                        state.next_report = now + repeat;
                    }
                    state = seen
                        .wake
                        .wait_timeout(state, tick)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            })
            .ok();
        Watch {
            shared,
            threshold,
            thread,
        }
    }

    /// The loop is on `file` (shown relative to `root`) at its first step.
    pub fn file(&self, root: &Path, file: &Path, step: &'static str) {
        let shown = file.strip_prefix(root).unwrap_or_else(|_| {
            // Never show a location outside the repo: keep the name only.
            file.file_name().map(Path::new).unwrap_or(file)
        });
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.file.clear();
        state.file.push_str(&shown.to_string_lossy());
        self.enter(&mut state, step);
    }

    /// The loop is past the files and in a named stage that has no file.
    pub fn stage(&self, stage: &'static str) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.file.clear();
        self.enter(&mut state, stage);
    }

    /// The loop moved to the next step of the same file.
    pub fn step(&self, step: &'static str) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        self.enter(&mut state, step);
    }

    fn enter(&self, state: &mut State, step: &'static str) {
        let now = Instant::now();
        state.step = step;
        state.since = now;
        state.next_report = now + self.threshold;
        state.idle = false;
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.done = true;
            state.idle = true;
        }
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect() -> (Arc<Mutex<Vec<String>>>, impl Fn(String) + Send + 'static) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        (lines, move |line| sink.lock().unwrap().push(line))
    }

    #[test]
    fn a_step_past_the_threshold_names_its_file_and_step() {
        let (lines, emit) = collect();
        let watch = Watch::start(Duration::from_millis(40), Duration::from_millis(60), emit);
        watch.file(
            Path::new("/work/repo"),
            Path::new("/work/repo/src/a/big.ts"),
            "definitions",
        );
        std::thread::sleep(Duration::from_millis(400));
        drop(watch);
        let lines = lines.lock().unwrap();
        assert!(lines.len() >= 2, "reports once, then again: {lines:?}");
        assert!(
            lines[0].starts_with("discovery: still reading src/a/big.ts (step definitions) after "),
            "{lines:?}"
        );
        assert!(!lines[0].contains("/work"), "{lines:?}");
    }

    #[test]
    fn the_slow_file_line_reaches_the_terminal_and_the_log() {
        let (terminal, file) = crate::logging::written_by(|| {
            let watch = Watch::discovery_at(Duration::from_millis(20), Duration::from_secs(5));
            watch.file(
                Path::new("/work/repo"),
                Path::new("/work/repo/src/a/big.ts"),
                "request summary",
            );
            std::thread::sleep(Duration::from_millis(300));
            drop(watch);
        });
        let want = "still reading src/a/big.ts (step request summary)";
        assert!(terminal.contains(want), "terminal: {terminal}");
        assert!(file.contains(want), "file: {file}");
        assert!(!terminal.contains("/work"), "{terminal}");
    }

    #[test]
    fn a_fast_discovery_prints_nothing() {
        let (terminal, file) = crate::logging::written_by(|| {
            let watch = Watch::discovery_at(Duration::from_secs(5), Duration::from_secs(5));
            for n in 0..50 {
                watch.file(Path::new("/r"), Path::new(&format!("/r/f{n}.ts")), "parse");
            }
            watch.stage("call edges");
            std::thread::sleep(Duration::from_millis(100));
            drop(watch);
        });
        assert!(!terminal.contains("discovery:") && !file.contains("discovery:"));
    }

    #[test]
    fn a_stage_past_the_threshold_is_named_without_a_file() {
        let (lines, emit) = collect();
        let watch = Watch::start(Duration::from_millis(30), Duration::from_secs(5), emit);
        watch.file(Path::new("/r"), Path::new("/r/a.ts"), "parse");
        watch.stage("call edges");
        std::thread::sleep(Duration::from_millis(300));
        drop(watch);
        let lines = lines.lock().unwrap();
        assert!(
            lines[0].starts_with("discovery: still in call edges after "),
            "{lines:?}"
        );
        assert!(!lines[0].contains("a.ts"), "{lines:?}");
    }

    #[test]
    fn a_fast_run_logs_nothing() {
        let (lines, emit) = collect();
        let watch = Watch::start(Duration::from_secs(5), Duration::from_secs(5), emit);
        for n in 0..200 {
            watch.file(Path::new("/r"), Path::new(&format!("/r/f{n}.ts")), "parse");
            watch.step("definitions");
        }
        std::thread::sleep(Duration::from_millis(100));
        drop(watch);
        assert!(lines.lock().unwrap().is_empty());
    }

    #[test]
    fn moving_on_resets_the_clock() {
        let (lines, emit) = collect();
        let watch = Watch::start(Duration::from_millis(200), Duration::from_secs(5), emit);
        for _ in 0..8 {
            watch.file(Path::new("/r"), Path::new("/r/a.ts"), "parse");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(watch);
        assert!(lines.lock().unwrap().is_empty());
    }

    #[test]
    fn a_path_outside_the_root_shows_only_its_name() {
        let (lines, emit) = collect();
        let watch = Watch::start(Duration::from_millis(20), Duration::from_secs(5), emit);
        watch.file(Path::new("/r"), Path::new("/elsewhere/owner/x.ts"), "parse");
        std::thread::sleep(Duration::from_millis(200));
        drop(watch);
        let lines = lines.lock().unwrap();
        assert!(lines[0].contains("reading x.ts (step parse)"), "{lines:?}");
    }
}
