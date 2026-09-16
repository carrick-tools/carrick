//! Local read-only mode: `carrick index | touch | check | refresh` (carrick#708).
//!
//! Local scans recompute deterministic facts without model calls or uploads.
//! Explicit index/refresh commands may read authenticated hosted indexes and
//! replay unchanged files' model answers through the incremental pipeline.
//! Hosted-only repositories participate in matching and type checks; local
//! navigation remains restricted to files in the selected workspace.
//!
//! `index` and `refresh` write; `touch` and `check` only read, in well under
//! the 300 ms an editor hook can afford, because everything they answer was
//! computed at index time. The exception is `check --recheck`, which re-judges
//! one edited file from the working tree inside a budget and still writes
//! nothing (carrick#1036, [`recheck`]).
//!
//! The output contract these commands print — the `carrick.check/0` JSON and
//! the human form beside it — is `docs/local-mode-output.md`. It is read by
//! surfaces outside this repo, so fields are added there, never renamed.

use std::path::Path;

pub mod cli;
mod contract;
pub(crate) mod hosted;
pub(crate) mod index;
pub(crate) mod jobs;
mod join;
pub(crate) mod query;
mod read_model;
pub(crate) mod recheck;
pub(crate) mod scan_state;
mod workspace;

pub use join::LocalJoin;

/// Set to `1` to run the pipeline with its model stage switched off: no
/// framework detection, no guidance, no file-analyzer dispatch. Deterministic
/// rows are emitted exactly as they are on every other path — a file that is
/// not dispatched is not a file that failed, so `scan_health` records nothing
/// and the run does not report a partial index.
pub const NO_MODEL_ENV: &str = "CARRICK_NO_MODEL";

/// Set to `1` to stop a scan uploading the index it builds.
///
/// One caller: a resume that found the cloud holding a NEWER index than the
/// commit it is finishing at. The local read model is worth building — it is
/// what `carrick check` answers from — but replacing a newer stored index with
/// an older one is not (carrick#1229).
pub const SKIP_UPLOAD_ENV: &str = "CARRICK_SKIP_UPLOAD";

/// Set to a path to make a cross-repo run write [`LocalJoin`] there and exit
/// instead of printing the report. The local indexer's join phase.
pub const JOIN_OUT_ENV: &str = "CARRICK_LOCAL_JOIN_OUT";

/// Set to `1` to skip signature inference (`signature_pass`).
///
/// A scan pays the sidecar once per unannotated parameter and return slot in
/// the whole repo to compose `FunctionDefinition.signature`, which serves the
/// hosted function index and nothing a contract verdict reads. Measured on a
/// 123-file package: 4 s of scan and 9 s of signature inference (carrick#1036).
/// The re-check behind an edit sets this because it has ten seconds for the
/// whole answer and reads no signature; a scan that uploads never sets it.
pub const SKIP_SIGNATURES_ENV: &str = "CARRICK_SKIP_SIGNATURES";

/// Whether this process runs without its model stage. Read from the
/// environment rather than threaded through the pipeline because the local
/// indexer drives the scan as a subprocess, exactly as the offline eval
/// harness does.
pub fn no_model() -> bool {
    std::env::var(NO_MODEL_ENV).as_deref() == Ok("1")
}

/// Whether this process composes signatures without asking the sidecar to
/// infer the slots the source left unannotated. See [`SKIP_SIGNATURES_ENV`].
pub fn skip_signature_inference() -> bool {
    // A dispatched run composes no signatures for the same reason it generates
    // no intents: it writes no index for them to live in, and inferring the
    // slots the source left unannotated is a sidecar pass over every function
    // in the service (carrick#1229).
    std::env::var(SKIP_SIGNATURES_ENV).as_deref() == Ok("1")
        || crate::analysis_channel::has_prompts()
}

/// The guidance map a no-model run analyses with: one entry per LLM-routed
/// protocol, carrying no patterns.
///
/// The file orchestrator requires an HTTP entry to exist (its absence is a
/// programming error, not a state), and every field it reads is prompt
/// material that no prompt will be built from here. So the map is present and
/// empty, which is the true statement: this run asked for no guidance.
pub fn offline_guidance() -> crate::agents::framework_guidance_agent::ProtocolGuidance {
    use crate::agents::framework_guidance_agent::{FrameworkGuidance, ProtocolGuidance};
    let mut guidance = ProtocolGuidance::new();
    guidance.insert(
        crate::operation::Protocol::Http,
        FrameworkGuidance {
            mount_patterns: Vec::new(),
            endpoint_patterns: Vec::new(),
            middleware_patterns: Vec::new(),
            data_fetching_patterns: Vec::new(),
            triage_hints: String::new(),
            parsing_notes: String::new(),
            // No guidance was asked for, so there is no stored entry to name.
            guidance_key: None,
        },
    );
    guidance
}

/// The line every local surface ends on, and the reason a local index holds
/// no candidates. Kept here so the renderer and the index summary say the same
/// thing.
pub const NOT_CLASSIFIED_LOCALLY: &str =
    "candidates: not classified locally (no model runs on this machine)";

/// Split a `"file:line"` or `"file:line:col"` location into its parts.
/// Delegates to the parser the type manifest already keys on, so a local row
/// and an indexed row agree on where something is.
fn split_location(location: &str) -> (String, Option<u32>) {
    let (file, line) = crate::type_manifest::parse_file_location(location);
    // The parser hands back the whole input as the path, and line 1, when the
    // location carries no line at all. A local row says "no line recorded"
    // rather than pointing a reader at the top of the file.
    if file == location {
        return (file, None);
    }
    (file, Some(line))
}

/// A path as the index records it: repo-relative, forward slashes, no leading
/// `./`.
fn normalize_relative(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.trim_start_matches("./").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_without_a_line_reports_none() {
        assert_eq!(
            split_location("src/app.ts"),
            ("src/app.ts".to_string(), None)
        );
    }

    #[test]
    fn location_with_a_line_reports_it() {
        assert_eq!(
            split_location("src/app.ts:42"),
            ("src/app.ts".to_string(), Some(42))
        );
    }

    #[test]
    fn location_with_line_and_column_keeps_the_line() {
        assert_eq!(
            split_location("src/app.ts:42:7"),
            ("src/app.ts".to_string(), Some(42))
        );
    }

    #[test]
    fn guidance_has_the_http_entry_the_orchestrator_requires() {
        // The orchestrator treats a missing HTTP entry as a hard error, so an
        // empty map would abort every no-model scan.
        assert!(offline_guidance().contains_key(&crate::operation::Protocol::Http));
    }
}
