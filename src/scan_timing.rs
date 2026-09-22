//! What a build spends its wall clock on, and the sentences it says about it
//! before the wait (carrick#1452).
//!
//! A user cannot tell which part of a scan is the model and which is the local
//! read, nor whether the cache is warm, so a five-minute rescan reads as "this
//! could be an hour". Two facts the scanner already holds answer that, and both
//! are said before the work starts: how big the tree is and how long the last
//! read of it took, and how much of the model analysis is already answered.
//!
//! Time and counts only. What a run costs us is our figure and never reaches a
//! customer's terminal (carrick#1236).
//!
//! The last run's MEASURED time is the estimate. Nothing here projects a rate
//! onto work that has no measured rate (carrick#1365): a tree that has never
//! been read says so instead of guessing.
//!
//! Three pieces, in the order they run:
//!
//! * A scan records its own split as it goes ([`service_analysed`],
//!   [`files_read`], [`uploaded`]) and states it to the parent that started it
//!   ([`report`]), on the same channel and for the same reason as its spend:
//!   the indexer swallows a scan's output.
//! * The build sums those splits, writes them beside the index
//!   ([`LastRead::write`]) and prints them.
//! * The next build reads that record back and opens with [`tree_line`].

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The stderr line a scan states its split on, for the parent that started it.
/// Chosen to be something no log line starts with, like the progress marker.
const MARKER: &str = "@carrick-timing ";

/// One run's wall clock, split the way the person waiting on it experiences
/// it: the tree being read, the model being asked, the answer going up.
///
/// Seconds rather than a duration, because this is written to a file a later
/// version reads back and adds up.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub struct Split {
    /// Files the deterministic pass read, across every service.
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub services: usize,
    /// Everything that happens on this machine: discovery, the SWC pass, the
    /// graph, the protocol scans, the type check.
    #[serde(default)]
    pub local_secs: f64,
    /// The model stages: file analysis and the function intents beside it.
    #[serde(default)]
    pub model_secs: f64,
    #[serde(default)]
    pub upload_secs: f64,
}

impl Split {
    /// Fold another scan's split into this one. A build runs one scan per repo
    /// and the reader is waiting on all of them.
    pub fn add(&mut self, other: &Split) {
        self.files += other.files;
        self.services += other.services;
        self.local_secs += other.local_secs;
        self.model_secs += other.model_secs;
        self.upload_secs += other.upload_secs;
    }

    /// Whether anything at all was measured. A split of zeroes is a run that
    /// did not record, and recording it would make the next run's "last time"
    /// a lie.
    pub fn measured(&self) -> bool {
        self.services > 0 || self.local_secs > 0.0 || self.model_secs > 0.0
    }
}

/// The record beside the index: the last completed build's split.
///
/// Beside `index.json` rather than inside it for the same reason the spend
/// receipt is: a free re-index rebuilds the read model from scratch and would
/// drop the figure.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct LastRead {
    /// RFC 3339, when the build that measured this finished.
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub split: Split,
}

impl LastRead {
    /// Read what a previous build left, or nothing where there is none.
    pub fn read(file: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Write the record where a reader can never see half of it.
    pub fn write(split: &Split, file: &std::path::Path) {
        let record = LastRead {
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            split: *split,
        };
        let Ok(json) = serde_json::to_vec_pretty(&record) else {
            return;
        };
        let pending = file.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&pending, json).is_ok() {
            let _ = std::fs::rename(&pending, file);
        }
        let _ = std::fs::remove_file(pending);
    }
}

/// What this process has measured so far.
///
/// The counts are held per service and the seconds are held as one sum,
/// because a service can be analysed TWICE in one scan: a service that owed
/// model work is retried once after a wait (`durability`), and the second pass
/// walks the same tree again. That second walk really did take time — it
/// belongs in the seconds — and it is not a second tree, so the size of the
/// tree must not double behind it.
#[derive(Debug, Default)]
struct Recorded {
    /// Files read, per service, replaced rather than added.
    files: std::collections::BTreeMap<String, usize>,
    local_secs: f64,
    model_secs: f64,
    upload_secs: f64,
}

impl Recorded {
    fn files_read(&mut self, service: String, files: usize) {
        self.files.insert(service, files);
    }

    fn analysed(&mut self, local_secs: f64, model_secs: f64) {
        self.local_secs += local_secs;
        self.model_secs += model_secs;
    }

    fn uploaded(&mut self, secs: f64) {
        self.upload_secs += secs;
    }

    fn split(&self) -> Split {
        Split {
            files: self.files.values().sum(),
            services: self.files.len(),
            local_secs: self.local_secs,
            model_secs: self.model_secs,
            upload_secs: self.upload_secs,
        }
    }
}

/// Process-global for the same reason [`crate::phase_timing`] is: the three
/// stages sit in three different call layers, and a value threaded through
/// them would have to cross every one. A scan is one repo, so a process's
/// marks belong to exactly one scan.
static RECORDED: Mutex<Option<Recorded>> = Mutex::new(None);

fn record(mark: impl FnOnce(&mut Recorded)) {
    if let Ok(mut recorded) = RECORDED.lock() {
        mark(recorded.get_or_insert_with(Recorded::default));
    }
}

/// Count the files one service's deterministic pass is about to read.
pub fn files_read(files: usize) {
    let service = crate::current_service::name().unwrap_or_default();
    record(|recorded| recorded.files_read(service, files));
}

/// Attribute one service's analysis: what it spent on this machine, and what
/// it spent waiting on the model.
pub fn service_analysed(local_secs: f64, model_secs: f64) {
    record(|recorded| recorded.analysed(local_secs, model_secs));
}

/// Attribute the wait on the upload.
pub fn uploaded(secs: f64) {
    record(|recorded| recorded.uploaded(secs));
}

/// What this process measured, and the end of recording.
pub fn take() -> Split {
    RECORDED
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .map(|recorded| recorded.split())
        .unwrap_or_default()
}

/// Hand this scan's split to the parent that started it, when there is one.
///
/// The indexer swallows a scan's stdout, so it crosses on stderr as JSON,
/// exactly as progress and spend do. A scan nobody is parsing has nowhere to
/// put it and says nothing.
pub fn report() {
    let split = take();
    if !crate::progress::parent_is_reading() || !split.measured() {
        return;
    }
    if let Ok(line) = serde_json::to_string(&split) {
        crate::errln!("{MARKER}{line}");
    }
}

/// Read one split out of a line of a scan's stderr, if that is what it is.
pub fn parse(line: &str) -> Option<Split> {
    let payload = line.trim_start().strip_prefix(MARKER)?;
    serde_json::from_str(payload).ok()
}

/// A wall time as a person reads one, in the shape the renderer already uses
/// for the step lines beside it (`elapsed` in `npm/carrick/src/scan.ts`): a
/// tenth of a second under a minute, minutes and whole seconds above it.
///
/// One shape, because the two halves of this appear in one run — the opening
/// line is written here and the closing one by the renderer — and a reader
/// comparing "last time" with what this run took must not have to convert
/// between them.
pub fn duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0.0s".to_string();
    }
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    // Rounded to the second first, so a run of 119.7s is `1m60s` in neither
    // half of the product.
    let total = secs.round() as u64;
    format!("{}m{}s", total / 60, total % 60)
}

/// The line a build opens with: how big this tree is and how long the last
/// read of it took.
///
/// A tree nobody has read has no measurement, and nothing here invents one.
pub fn tree_line(previous: Option<&LastRead>) -> String {
    let Some(split) = previous
        .map(|record| &record.split)
        .filter(|s| s.measured())
    else {
        return "Reading the tree: first read of this tree.".to_string();
    };
    format!(
        "Reading the tree: {} across {}; last time {}.",
        plural(split.files, "file"),
        plural(split.services, "service"),
        duration(split.local_secs),
    )
}

/// The line a scan says once its cache check has decided and before it asks
/// the model anything.
///
/// The unit is files, because a file is what the analyzer is asked about and
/// what its cache is keyed on: these two numbers ARE the decision that has
/// just been made, and a function count derived from them would be a number no
/// cache decided (carrick#1452).
pub fn model_line(already_analysed: usize, fresh: usize) -> String {
    let total = already_analysed + fresh;
    if total == 0 {
        return "Model analysis: nothing here needs the model.".to_string();
    }
    if fresh == 0 {
        return format!(
            "Model analysis: all {} already analysed.",
            plural(total, "file")
        );
    }
    format!(
        "Model analysis: {already_analysed} of {} already analysed, {fresh} new.",
        plural(total, "file")
    )
}

/// What a finished build measured, for the reader who just waited through it.
/// The same three figures the next run's opening line is read from.
pub fn split_line(split: &Split) -> String {
    format!(
        "local read {} · model analysis {} · upload {}",
        duration(split.local_secs),
        duration(split.model_secs),
        duration(split.upload_secs),
    )
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        return format!("{count} {noun}");
    }
    format!("{count} {noun}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wall_time_is_stated_in_the_units_a_person_reads() {
        assert_eq!(duration(0.0), "0.0s");
        assert_eq!(duration(43.4), "43.4s");
        assert_eq!(duration(59.6), "59.6s");
        assert_eq!(duration(119.7), "2m0s");
        assert_eq!(duration(133.0), "2m13s");
        assert_eq!(duration(-1.0), "0.0s");
    }

    /// A tree nobody has read says so. The estimate is the last measurement
    /// and there is no other kind (carrick#1365).
    #[test]
    fn a_tree_with_no_record_is_not_given_an_estimate() {
        assert_eq!(
            tree_line(None),
            "Reading the tree: first read of this tree."
        );
        let unmeasured = LastRead::default();
        assert_eq!(
            tree_line(Some(&unmeasured)),
            "Reading the tree: first read of this tree."
        );
    }

    #[test]
    fn a_tree_that_was_read_before_states_its_size_and_that_time() {
        let record = LastRead {
            updated_at: "2026-09-22T09:00:00Z".to_string(),
            split: Split {
                files: 1204,
                services: 5,
                local_secs: 133.0,
                model_secs: 200.0,
                upload_secs: 12.0,
            },
        };
        assert_eq!(
            tree_line(Some(&record)),
            "Reading the tree: 1204 files across 5 services; last time 2m13s."
        );
    }

    /// The three forms of the cache-check sentence, cold, partial and warm.
    #[test]
    fn the_model_line_says_what_the_cache_check_decided() {
        assert_eq!(
            model_line(0, 1204),
            "Model analysis: 0 of 1204 files already analysed, 1204 new."
        );
        assert_eq!(
            model_line(1100, 104),
            "Model analysis: 1100 of 1204 files already analysed, 104 new."
        );
        assert_eq!(
            model_line(1204, 0),
            "Model analysis: all 1204 files already analysed."
        );
        assert_eq!(
            model_line(1, 0),
            "Model analysis: all 1 file already analysed."
        );
        assert_eq!(
            model_line(0, 0),
            "Model analysis: nothing here needs the model."
        );
    }

    #[test]
    fn a_split_survives_the_crossing_from_the_scan_that_measured_it() {
        let split = Split {
            files: 900,
            services: 3,
            local_secs: 61.5,
            model_secs: 200.25,
            upload_secs: 4.0,
        };
        let line = format!("{MARKER}{}", serde_json::to_string(&split).unwrap());
        assert_eq!(parse(&line), Some(split));
        assert!(parse("@carrick-timing not json").is_none());
        assert!(parse("Uploading results...").is_none());
        assert!(parse("").is_none());
    }

    /// The build writes what it measured and the next build reads it back:
    /// that round trip is what makes "last time" a measurement rather than a
    /// guess.
    #[test]
    fn the_record_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("last-read.json");
        assert_eq!(LastRead::read(&file), None);
        let split = Split {
            files: 1204,
            services: 5,
            local_secs: 133.0,
            model_secs: 200.0,
            upload_secs: 12.0,
        };
        LastRead::write(&split, &file);
        let read = LastRead::read(&file).expect("the record is there");
        assert_eq!(read.split, split);
        assert!(!read.updated_at.is_empty());
        assert_eq!(
            tree_line(Some(&read)),
            "Reading the tree: 1204 files across 5 services; last time 2m13s."
        );
    }

    /// A service that owed model work is analysed a second time in the same
    /// scan, and walks the same files again. The second walk's seconds are
    /// part of the wait; its files are not a second tree.
    ///
    /// Driven over the recorder's own type rather than the process-global one
    /// behind it: every other test in this binary that runs an upload or an
    /// analysis writes to that global, and a test reading it would be reading
    /// their scan as well as its own.
    #[test]
    fn a_service_analysed_twice_is_one_service_and_both_waits() {
        let mut recorded = Recorded::default();
        assert_eq!(recorded.split(), Split::default());

        recorded.files_read("api".to_string(), 40);
        recorded.analysed(1.0, 9.0);
        // The retry: the same service, the same files, more wall clock.
        recorded.files_read("api".to_string(), 40);
        recorded.analysed(0.5, 4.0);

        recorded.files_read("web".to_string(), 12);
        recorded.analysed(2.0, 0.0);
        recorded.uploaded(3.0);

        assert_eq!(
            recorded.split(),
            Split {
                files: 52,
                services: 2,
                local_secs: 3.5,
                model_secs: 13.0,
                upload_secs: 3.0,
            }
        );
    }

    #[test]
    fn splits_add_up_across_the_repos_of_one_build() {
        let mut total = Split::default();
        assert!(!total.measured());
        total.add(&Split {
            files: 10,
            services: 1,
            local_secs: 1.0,
            model_secs: 2.0,
            upload_secs: 3.0,
        });
        total.add(&Split {
            files: 5,
            services: 2,
            local_secs: 0.5,
            model_secs: 0.25,
            upload_secs: 0.0,
        });
        assert_eq!(
            total,
            Split {
                files: 15,
                services: 3,
                local_secs: 1.5,
                model_secs: 2.25,
                upload_secs: 3.0,
            }
        );
        assert!(total.measured());
        assert_eq!(
            split_line(&total),
            "local read 1.5s · model analysis 2.2s · upload 3.0s"
        );
    }
}
