//! Where a service's analysis phase spent its wall clock.
//!
//! The per-service line already stated `packages`, `sidecar` and `analysis`,
//! and `analysis` was everything: on a warm rescan of a 33-service monorepo
//! that one number was 774 s in one release and 1,024 s in the next with the
//! same tree, and nothing in the log said which part of it grew (carrick#767).
//! This records the elapsed time of each stage inside that phase so the line
//! attributes itself.
//!
//! It is a process-global recorder rather than a value threaded through the
//! analysis, for the same reason [`crate::scan_health`] is: the stages sit in
//! two branches (incremental and full) several call layers below the loop that
//! prints the line, and a return value would have to be carried through every
//! one of them. Services are analysed one at a time, so a run's marks belong
//! to exactly one service.
//!
//! The rendered line is a contract with the rescan gate, which parses
//! `analysis <seconds>s` per service and refuses a run whose total has grown
//! against the previous scan of the same tree. Keep the field order and the
//! `name 0.0s` shape stable.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The stages of one service's analysis, in the order the line prints them.
///
/// Every stage is printed on every service, including the ones that cost
/// nothing: a phase missing from the line reads as "not measured", and the
/// point of the line is that the seconds add up to `analysis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// File discovery and the SWC pass over them (parse).
    Discover,
    /// Reading the previous scan's answers and deciding what is reusable.
    Cache,
    /// The file-analyzer stage: deterministic emission, the model calls this
    /// scan dispatches, and the join of the two.
    Model,
    /// Mount-graph construction.
    Graph,
    /// The deterministic protocol scans (GraphQL, sockets, pub/sub).
    Protocols,
    /// Function-intent generation.
    Intents,
    /// Function-signature composition, including sidecar inference.
    Signatures,
    /// Assembling the payload from the graph and the two whole-workspace
    /// passes over it: external call candidates and the SDK surface.
    Surface,
    /// Type-manifest assembly and the collection of the protocol type
    /// requests.
    Manifest,
    /// Sidecar type resolution: the inference and bundle round trips, and the
    /// scanner-side collection of the requests they carry.
    Types,
    /// The v2 capture round trip (and the backfill re-run when it fires).
    Capture,
    /// Per-endpoint definition resolution from the capture stub.
    Definitions,
    /// Everything else between the marks above.
    Other,
}

impl Phase {
    const ORDER: [Phase; 13] = [
        Phase::Discover,
        Phase::Cache,
        Phase::Model,
        Phase::Graph,
        Phase::Protocols,
        Phase::Intents,
        Phase::Signatures,
        Phase::Surface,
        Phase::Manifest,
        Phase::Types,
        Phase::Capture,
        Phase::Definitions,
        Phase::Other,
    ];

    fn label(self) -> &'static str {
        match self {
            Phase::Discover => "discover",
            Phase::Cache => "cache",
            Phase::Model => "model",
            Phase::Graph => "graph",
            Phase::Protocols => "protocols",
            Phase::Intents => "intents",
            Phase::Signatures => "signatures",
            Phase::Surface => "surface",
            Phase::Manifest => "manifest",
            Phase::Types => "types",
            Phase::Capture => "capture",
            Phase::Definitions => "definitions",
            Phase::Other => "other",
        }
    }
}

#[derive(Debug)]
struct Recorder {
    /// When the stage currently being timed started.
    last: Instant,
    totals: BTreeMap<Phase, f64>,
}

fn recorder() -> &'static Mutex<Option<Recorder>> {
    static RECORDER: OnceLock<Mutex<Option<Recorder>>> = OnceLock::new();
    RECORDER.get_or_init(|| Mutex::new(None))
}

/// Begin recording a service's analysis. Discards anything the previous
/// service left behind, so a stage that never marked cannot leak forward.
pub fn start_service() {
    let mut guard = recorder().lock().unwrap();
    *guard = Some(Recorder {
        last: Instant::now(),
        totals: BTreeMap::new(),
    });
}

/// Attribute everything since the previous mark to `phase`.
///
/// A phase may be marked more than once (the incremental branch resolves
/// types, then definitions, then more types on some services); the durations
/// add.
pub fn mark(phase: Phase) {
    let mut guard = recorder().lock().unwrap();
    let Some(recorder) = guard.as_mut() else {
        return;
    };
    let now = Instant::now();
    let elapsed = now.duration_since(recorder.last).as_secs_f64();
    *recorder.totals.entry(phase).or_insert(0.0) += elapsed;
    recorder.last = now;
}

/// The breakdown as the per-service line prints it, and the end of recording.
///
/// Returns `None` when nothing was recorded (a path that never called
/// [`start_service`]), so the caller prints its line unchanged rather than an
/// all-zero breakdown that would read as a measurement.
pub fn take_line() -> Option<String> {
    let mut guard = recorder().lock().unwrap();
    let recorder = guard.take()?;
    let parts: Vec<String> = Phase::ORDER
        .iter()
        .map(|phase| {
            format!(
                "{} {:.1}s",
                phase.label(),
                recorder.totals.get(phase).copied().unwrap_or(0.0)
            )
        })
        .collect();
    Some(parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line names every phase in a fixed order whether or not it was
    /// marked, because the gate that parses it counts on the shape; and
    /// nothing recorded is stated as nothing, not as a service that spent
    /// zero seconds everywhere.
    ///
    /// One test, not two: the recorder is process-global, so two tests
    /// asserting on it in parallel would each see the other's service.
    #[test]
    fn line_states_every_phase_in_order_and_only_after_a_start() {
        start_service();
        mark(Phase::Discover);
        let line = take_line().expect("a started service renders a line");
        for phase in Phase::ORDER {
            assert!(line.contains(phase.label()), "{line} is missing {phase:?}");
        }
        let discover = line.find("discover").expect("discover present");
        let other = line.find("other").expect("other present");
        assert!(discover < other, "phases print in ORDER: {line}");

        assert!(take_line().is_none(), "a taken recording is finished");
    }
}
