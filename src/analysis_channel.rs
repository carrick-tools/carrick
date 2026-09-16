//! Where a scan's model answers come from, and where its prompts go
//! (carrick#1229).
//!
//! An ordinary scan asks the model and waits. Two other things can happen at
//! the same point in the pipeline, and this module is the one place that knows
//! which:
//!
//! * **Dispatch.** The scan builds every prompt, hands them to the collector
//!   here, and ends without an index. The cloud answers them on its own time.
//! * **Resume.** The scan holds an answer bundle, rebuilds each prompt, and
//!   takes the answer whose id matches the body it just built — a join on
//!   CONTENT, so it works on a dirty tree, in a shallow clone, at a later
//!   commit and on a machine that never saw the dispatch. Anything that does
//!   not match goes to the model in the ordinary way.
//!
//! A process-global rather than a parameter, for the same reason
//! [`crate::scan_health`] is one: the decision is made once, at the top of a
//! run, and read at one point deep inside the file orchestrator.
//!
//! The collector holds the rows in memory until the run ends. On the largest
//! repos that is a few hundred megabytes for the length of one dispatch, which
//! is a scan that is doing nothing else; streaming them to disk as they are
//! built is carrick#1244.
//!
//! Reference: `docs/reference/dispatch-resume.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::agents::file_analyzer_agent::AnalysisPrompt;
use crate::analysis_job::{AnalyzeRow, AnswerBundle, body_id, value_sha};

/// Set by the indexer on a repo scan it wants dispatched rather than run.
///
/// Asking is not deciding: the run dispatches only if the cloud also says it
/// takes analysis jobs, so a scanner in front of a cloud that has not deployed
/// them stays synchronous and nobody is left holding a bundle nothing reads.
pub const DISPATCH_ENV: &str = "CARRICK_DISPATCH";

/// Set by the indexer on a repo scan that is collecting a dispatched job's
/// answers: the path to the answer bundle on disk.
pub const ANSWERS_ENV: &str = "CARRICK_ANSWERS";

/// The prompts a dispatched run has built so far.
#[derive(Default)]
struct Collector {
    rows: Vec<AnalyzeRow>,
    /// `guidance_key` → the rendered block, carried once for every row that
    /// names it instead of once per row.
    guidance: BTreeMap<String, String>,
    /// `schema_sha` → the response schema, likewise.
    schemas: BTreeMap<String, serde_json::Value>,
    services: Vec<String>,
    /// Services this job cannot carry (see [`degrade`]).
    degraded: Vec<String>,
}

static COLLECTOR: Mutex<Option<Collector>> = Mutex::new(None);
/// The bundle in hand, and the path it was read from.
static ANSWERS: Mutex<Option<(PathBuf, Option<&'static AnswerBundle>)>> = Mutex::new(None);

/// Whether this run was ASKED to dispatch. Answered from the environment, so
/// it is readable before the cloud has been asked anything.
pub fn dispatch_requested() -> bool {
    std::env::var_os(DISPATCH_ENV).is_some_and(|value| !value.is_empty())
}

/// Start collecting: this run dispatches its prompts and writes no index.
///
/// Called once, after the cloud has said it accepts analysis jobs.
pub fn begin_dispatch() {
    if let Ok(mut collector) = COLLECTOR.lock() {
        *collector = Some(Collector::default());
    }
}

/// Whether prompts are being collected rather than sent.
pub fn dispatching() -> bool {
    COLLECTOR.lock().is_ok_and(|collector| collector.is_some())
}

/// Whether this run has collected anything yet.
///
/// The difference between "asked to dispatch" and "has something to hand
/// over", and it decides two things: a service that built no prompt runs its
/// remaining phases and its intents like any other scan, and a run that built
/// none at all finishes as an ordinary scan rather than handing over nothing
/// (carrick#1229).
pub fn has_prompts() -> bool {
    COLLECTOR
        .lock()
        .is_ok_and(|collector| collector.as_ref().is_some_and(|c| !c.rows.is_empty()))
}

/// Keep one file's prompt for the bundle.
///
/// The row carries the body and the two keys; the block and the schema are
/// registered once each, whatever how many files name them.
pub fn record(
    service: Option<&str>,
    guidance_key: &str,
    prompt: &AnalysisPrompt,
    schema: &serde_json::Value,
) {
    let Ok(mut guard) = COLLECTOR.lock() else {
        return;
    };
    let Some(collector) = guard.as_mut() else {
        return;
    };
    let service = service.unwrap_or_default().to_string();
    if !collector.services.contains(&service) {
        collector.services.push(service.clone());
    }
    let schema_sha = value_sha(schema);
    collector
        .schemas
        .entry(schema_sha.clone())
        .or_insert_with(|| schema.clone());
    collector
        .guidance
        .entry(guidance_key.to_string())
        .or_insert_with(|| prompt.guidance_block().to_string());
    collector.rows.push(AnalyzeRow {
        id: body_id(prompt.body()),
        service,
        guidance_key: guidance_key.to_string(),
        body: prompt.body().to_string(),
        schema_sha,
    });
}

/// Refuse this dispatch, naming what could not be carried.
///
/// One case today: a service whose framework guidance carries no id. The
/// cloud keys the whole message when the id is absent, so the block this
/// bundle carries once would be re-keyed per file and the job would buy every
/// answer twice. A run that cannot dispatch says so and is run as an ordinary
/// scan instead; it never ships a bundle it knows is wrong.
pub fn degrade(service: &str) {
    let Ok(mut guard) = COLLECTOR.lock() else {
        return;
    };
    let Some(collector) = guard.as_mut() else {
        return;
    };
    if !collector.degraded.contains(&service.to_string()) {
        collector.degraded.push(service.to_string());
    }
}

/// What a dispatched run collected, taken once at the end of it.
pub struct Collected {
    pub rows: Vec<AnalyzeRow>,
    pub guidance: BTreeMap<String, String>,
    pub schemas: BTreeMap<String, serde_json::Value>,
    pub services: Vec<String>,
    pub degraded: Vec<String>,
}

/// Take everything collected. `None` when this run was not dispatching.
pub fn take() -> Option<Collected> {
    let mut guard = COLLECTOR.lock().ok()?;
    let collector = guard.take()?;
    Some(Collected {
        rows: collector.rows,
        guidance: collector.guidance,
        schemas: collector.schemas,
        services: collector.services,
        degraded: collector.degraded,
    })
}

/// The answers this run is resuming with, read from [`ANSWERS_ENV`].
///
/// Read once per bundle and kept for the life of the process: a monorepo runs
/// this once per service, and re-reading a bundle of thousands of answers for
/// each of them would be the slowest thing in a resume. Keyed by path rather
/// than read once outright, so a process that is handed a second bundle reads
/// the second one instead of silently replaying the first.
///
/// A bundle that cannot be read is not a failed scan: the run says so and asks
/// the model for everything, which is what it would have done without one.
pub fn answers() -> Option<&'static AnswerBundle> {
    let path = PathBuf::from(std::env::var_os(ANSWERS_ENV)?);
    let mut guard = ANSWERS.lock().ok()?;
    if let Some((read, bundle)) = guard.as_ref()
        && read == &path
    {
        return *bundle;
    }
    let bundle = load(&path).map(|bundle| &*Box::leak(Box::new(bundle)));
    *guard = Some((path, bundle));
    bundle
}

fn load(path: &Path) -> Option<AnswerBundle> {
    match std::fs::read(path)
        .map_err(|e| format!("{}: {e}", path.display()))
        .and_then(|bytes| AnswerBundle::decode(&bytes))
    {
        // A job that died before its first pass hands back a header and
        // nothing else. Treat it as no bundle at all, so the scan does not
        // report itself as a resume of nothing.
        Ok(bundle) if bundle.is_empty() => {
            tracing::warn!(
                "The collected analysis holds no answers; this scan analyses every file itself"
            );
            None
        }
        Ok(bundle) => {
            tracing::info!(
                "Resuming with {} collected answer(s), {} of them already held by the cloud; {} \
                 row(s) the job could not answer",
                bundle.len(),
                bundle.cached_count(),
                bundle.failure_count(),
            );
            Some(bundle)
        }
        Err(error) => {
            tracing::warn!(
                "Could not read the collected answers ({error}); this scan analyses every file \
                 itself"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn prompt(block: &str, body: &str) -> AnalysisPrompt {
        AnalysisPrompt {
            text: format!("{block}{body}"),
            guidance_prefix_bytes: block.len(),
        }
    }

    #[test]
    fn a_prompt_splits_into_the_block_that_is_carried_once_and_the_body_that_is_not() {
        let prompt = prompt("## GUIDANCE\n", "### FILE\nconst a = 1;\n");
        assert_eq!(prompt.guidance_block(), "## GUIDANCE\n");
        assert_eq!(prompt.body(), "### FILE\nconst a = 1;\n");
        assert_eq!(
            prompt.guidance_block().len() + prompt.body().len(),
            prompt.text.len()
        );
    }

    /// Two files of one service carry the block once between them; that is the
    /// whole of the bundle's size argument.
    #[test]
    #[serial(analysis_channel)]
    fn the_collector_carries_each_constant_once_and_each_body_once() {
        let schema = serde_json::json!({"type": "object"});
        begin_dispatch();
        record(Some("api"), "key", &prompt("## G\n", "one"), &schema);
        record(Some("api"), "key", &prompt("## G\n", "two"), &schema);
        let collected = take().expect("dispatching");
        assert_eq!(collected.rows.len(), 2);
        assert_eq!(collected.guidance.len(), 1);
        assert_eq!(collected.schemas.len(), 1);
        assert_eq!(collected.services, vec!["api".to_string()]);
        assert_eq!(collected.rows[0].id, body_id("one"));
        assert_eq!(collected.rows[1].body, "two");
        assert!(!dispatching(), "taking ends the collection");
    }

    #[test]
    #[serial(analysis_channel)]
    fn nothing_is_recorded_by_a_run_that_is_not_dispatching() {
        let _ = take();
        record(
            Some("api"),
            "key",
            &prompt("## G\n", "one"),
            &serde_json::json!({}),
        );
        assert!(take().is_none());
    }
}
