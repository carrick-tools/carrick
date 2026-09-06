//! Local read-only mode: `carrick index | touch | check | refresh` (carrick#708).
//!
//! The same pipeline the Action runs, pointed at a folder of repos on a
//! developer's machine, with two differences that define the mode:
//!
//! * **No model.** There is no model on a laptop and no credential to reach
//!   one, so the scan runs its deterministic passes and stops. Every row the
//!   local index holds is a fact some pass states outright; a candidate the
//!   model would have classified is counted by the boundary and not guessed
//!   at. [`no_model`] is that switch, read in exactly three places (framework
//!   detection, guidance, and the file-analyzer dispatch).
//! * **No cloud.** The scanner authenticates with GitHub OIDC, which a laptop
//!   does not have, so the index is written to and read from `<workspace>/
//!   .carrick/` through the existing [`crate::cloud_storage::LocalDirStorage`]
//!   backend. Nothing is uploaded and nothing is downloaded.
//!
//! `index` and `refresh` write; `touch` and `check` only read, in well under
//! the 300 ms an editor hook can afford, because everything they answer was
//! computed at index time.
//!
//! The output contract these commands print — the `carrick.check/0` JSON and
//! the human form beside it — is `docs/local-mode-output.md`. It is read by
//! surfaces outside this repo, so fields are added there, never renamed.

use std::path::Path;

pub mod cli;
mod contract;
mod index;
mod join;
mod query;
mod read_model;
mod workspace;

pub use join::LocalJoin;

/// Set to `1` to run the pipeline with its model stage switched off: no
/// framework detection, no guidance, no file-analyzer dispatch. Deterministic
/// rows are emitted exactly as they are on every other path — a file that is
/// not dispatched is not a file that failed, so `scan_health` records nothing
/// and the run does not report a partial index.
pub const NO_MODEL_ENV: &str = "CARRICK_NO_MODEL";

/// Set to a path to make a cross-repo run write [`LocalJoin`] there and exit
/// instead of printing the report. The local indexer's join phase.
pub const JOIN_OUT_ENV: &str = "CARRICK_LOCAL_JOIN_OUT";

/// Whether this process runs without its model stage. Read from the
/// environment rather than threaded through the pipeline because the local
/// indexer drives the scan as a subprocess, exactly as the offline eval
/// harness does.
pub fn no_model() -> bool {
    std::env::var(NO_MODEL_ENV).as_deref() == Ok("1")
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
