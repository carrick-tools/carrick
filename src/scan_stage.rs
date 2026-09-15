//! Which stage of a scan this process is in (carrick#1063).
//!
//! A first index that died at minute 66 left the cloud with nothing but a scan
//! slot that never closed: no row said what the run had been doing when it
//! stopped. This is the answer to that question, in one short token — the
//! fail marker sends it as `stage`, and the scanner's own last stderr line
//! prints it, so a user who pastes their terminal has already reported it.
//!
//! A process-global for the same reason [`crate::phase_timing`] and
//! [`crate::scan_health`] are: the stages sit several call layers below the
//! function that has to name one, and threading a value through every one of
//! them would be a worse record than an atomic store. One process is one
//! scan, so there is exactly one answer to keep.
//!
//! The set is deliberately coarse and closed. It is a bucket a human reads in
//! a dashboard, not a trace: `file_analysis` is where a first index spends
//! most of its wall clock, and knowing a run died there rather than in
//! `upload` is the whole difference the marker exists to state. Finer
//! attribution is already in the log the same failure ships.

use std::sync::atomic::{AtomicU8, Ordering};

/// The stage a scan is in, as the cloud stores it.
///
/// Every label is snake_case and well under the 64-byte wire limit; the test
/// below pins both, because the cloud rejects anything else and a refused
/// marker is a marker that never arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Before the pipeline starts: the scan target, its services, the
    /// runtime they need, the type sidecar coming up, and the storage and
    /// credential the run will use. A failure here opens no scan, so it
    /// reaches the cloud as `preflight-failed` rather than as a fail marker
    /// (carrick#1096).
    Preflight,
    /// The pipeline's first stage: opening the run with the cloud and
    /// downloading what the project already holds.
    Discovery,
    /// Framework detection and the guidance that follows from it.
    FrameworkDetect,
    /// The file-analyzer stage: the deterministic pass and the model calls
    /// this scan dispatches. The long one on a first index.
    FileAnalysis,
    /// Function intents.
    Intents,
    /// Signature composition, including the sidecar inference behind it.
    Signatures,
    /// The sidecar's type work: resolution, bundling, and the v2 capture.
    TypeCapture,
    /// Per-endpoint definition resolution from the capture stub.
    Definitions,
    /// Cross-repo analysis and the type check over the joined blobs.
    CrossRepoCheck,
    /// Assembling the payloads the upload will carry.
    BlobBuild,
    /// The write actions themselves, and the log upload after them.
    Upload,
    /// Nothing has claimed a stage yet, or the process is past the last one.
    Unknown,
}

impl Stage {
    /// The wire token. snake_case, at most 64 bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Preflight => "preflight",
            Stage::Discovery => "discovery",
            Stage::FrameworkDetect => "framework_detect",
            Stage::FileAnalysis => "file_analysis",
            Stage::Intents => "intents",
            Stage::Signatures => "signatures",
            Stage::TypeCapture => "type_capture",
            Stage::Definitions => "definitions",
            Stage::CrossRepoCheck => "cross_repo_check",
            Stage::BlobBuild => "blob_build",
            Stage::Upload => "upload",
            Stage::Unknown => "unknown",
        }
    }

    /// Every stage, for the tests that hold the set to its contract.
    const ALL: [Stage; 12] = [
        Stage::Preflight,
        Stage::Discovery,
        Stage::FrameworkDetect,
        Stage::FileAnalysis,
        Stage::Intents,
        Stage::Signatures,
        Stage::TypeCapture,
        Stage::Definitions,
        Stage::CrossRepoCheck,
        Stage::BlobBuild,
        Stage::Upload,
        Stage::Unknown,
    ];

    fn from_code(code: u8) -> Stage {
        Stage::ALL
            .get(code as usize)
            .copied()
            .unwrap_or(Stage::Unknown)
    }

    fn code(self) -> u8 {
        Stage::ALL
            .iter()
            .position(|stage| *stage == self)
            .unwrap_or(Stage::ALL.len() - 1) as u8
    }
}

/// The stage this process is in. Starts at `unknown`, which is the truth
/// before the first `enter`.
static CURRENT: AtomicU8 = AtomicU8::new(11);

/// Say the scan has reached `stage`. Later calls replace earlier ones: a run
/// moves forward and the marker wants the last stage it reached, not the
/// first.
pub fn enter(stage: Stage) {
    CURRENT.store(stage.code(), Ordering::Relaxed);
}

/// The stage this process is in.
pub fn current() -> Stage {
    Stage::from_code(CURRENT.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cloud stores `stage` as a short snake_case token and refuses
    /// anything else, so a label that stops being one is a marker that never
    /// arrives — silently, because the marker is best-effort by design.
    #[test]
    fn every_label_is_a_short_snake_case_token() {
        for stage in Stage::ALL {
            let label = stage.as_str();
            assert!(!label.is_empty(), "{stage:?} has no label");
            assert!(label.len() <= 64, "{label} is longer than the wire allows");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "{label} is not snake_case"
            );
        }
    }

    /// One label per stage. Two stages sharing one would make the marker
    /// answer a question it was not asked.
    #[test]
    fn no_two_stages_share_a_label() {
        let mut labels: Vec<&str> = Stage::ALL.iter().map(|s| s.as_str()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "duplicate label in {labels:?}");
    }

    /// The global round-trips every stage, and starts where an unclaimed
    /// process should: `unknown`.
    #[test]
    fn the_global_round_trips_every_stage() {
        assert_eq!(Stage::from_code(CURRENT.load(Ordering::Relaxed)), current());
        for stage in Stage::ALL {
            enter(stage);
            assert_eq!(current(), stage);
        }
        // An encoding no release wrote reads as unknown rather than as
        // whichever stage happens to sit at that index.
        assert_eq!(Stage::from_code(200), Stage::Unknown);
        enter(Stage::Unknown);
    }

    /// The initial value of the atomic must be the code `Unknown` encodes —
    /// it is written as a literal because `code()` is not const.
    #[test]
    fn the_atomic_starts_at_unknown() {
        assert_eq!(Stage::Unknown.code(), 11);
        assert_eq!(Stage::from_code(11), Stage::Unknown);
    }
}
