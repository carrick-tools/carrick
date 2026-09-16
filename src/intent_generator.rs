//! Function intent generator.
//!
//! Generates short natural-language descriptions of what each function
//! intends to do, using a small LLM model. Functions are processed in
//! dependency order (leaves first) so that when a function calls other
//! local functions, those functions' intents are included in the prompt
//! for richer compositional understanding.
//!
//! After intent generation, `body_source` is stripped from all function
//! definitions so that source code is not uploaded to AWS. The intent
//! serves as the index; GitHub is the source of truth for code.

use crate::agent_service::{AgentCallError, AgentService, rate_limit_tripped};
use crate::visitor::FunctionDefinition;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, warn};

/// Bump when the `/generate-intent` model or prompt template changes so that
/// intents cached by content hash are regenerated rather than reused. The model
/// and prompt live in the lambda (carrick-cloud), invisible to this crate, so
/// this constant is the manual invalidation lever.
// v2: the /generate-intent lambda moved from the AI Studio gemini-3-flash-preview
// model to Vertex AI gemini-3.1-flash-lite (carrick-cloud#140). Bumping forces a
// one-time regeneration of every cached intent on the first post-switch scan.
const INTENT_CACHE_VERSION: u32 = 2;

/// Content hash of the exact inputs that determine a function's generated
/// intent: the cache version, the function body, and its callees' intents.
/// Callee intents are sorted so set-equal contexts hash identically regardless
/// of discovery order. Fields are length-delimited so concatenation is
/// unambiguous.
fn compute_intent_hash(body: &str, called_intents: &[String]) -> String {
    let mut sorted: Vec<&String> = called_intents.iter().collect();
    sorted.sort();

    let mut hasher = Sha256::new();
    hasher.update(INTENT_CACHE_VERSION.to_le_bytes());
    hasher.update((body.len() as u64).to_le_bytes());
    hasher.update(body.as_bytes());
    hasher.update((sorted.len() as u64).to_le_bytes());
    for ci in sorted {
        hasher.update((ci.len() as u64).to_le_bytes());
        hasher.update(ci.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// What the previous scan of a service left for the intent pass.
///
/// `by_hash` is the content-addressed cache: a function whose freshly computed
/// hash is in it (same body, same callee intents) reuses that intent without a
/// `/generate-intent` call. Only entries carrying both an intent and the hash
/// that produced it go in; a row the cloud carried forward has no hash and is a
/// miss.
///
/// `by_key` is each function's previous intent by definition key, for a
/// function this run defers (carrick#1080): one whose callee got no intent is
/// not keyed at all, and keeps what it had. The key is the whole identity: a
/// name two files define is keyed with its repo-relative path (#582), so a
/// key names the same symbol on both scans.
#[derive(Debug, Clone, Default)]
pub struct PreviousIntents {
    by_hash: HashMap<String, String>,
    by_key: HashMap<String, PreviousIntent>,
}

#[derive(Debug, Clone)]
struct PreviousIntent {
    intent: String,
    hash: Option<String>,
}

impl PreviousIntents {
    /// Read a previous scan's function definitions.
    pub fn from_definitions(function_definitions: &HashMap<String, FunctionDefinition>) -> Self {
        let mut previous = Self::default();
        for (key, def) in function_definitions {
            let Some(intent) = &def.intent else {
                continue;
            };
            if let Some(hash) = &def.intent_input_hash {
                previous.by_hash.insert(hash.clone(), intent.clone());
            }
            previous.by_key.insert(
                key.clone(),
                PreviousIntent {
                    intent: intent.clone(),
                    hash: def.intent_input_hash.clone(),
                },
            );
        }
        previous
    }

    /// The intent a previous scan generated from exactly these inputs.
    pub fn for_hash(&self, hash: &str) -> Option<&str> {
        self.by_hash.get(hash).map(String::as_str)
    }

    /// The previous intent of the function at `key`, with the hash that
    /// produced it if it has one.
    fn for_function(&self, key: &str) -> Option<&PreviousIntent> {
        self.by_key.get(key)
    }
}

/// Intents described earlier in this run, shared across the services of one
/// scan (carrick#1080 D6).
///
/// Keyed on the request's `name` and the content hash, which covers the body
/// and the callee intents: exactly what the cloud's intent cache key reads, so
/// a hit is a request another service already made or replayed from its own
/// previous scan. A workspace whose members hold the same function describes
/// it once, and its callers then see the same callee intent in every member,
/// so they dedupe too. The key material itself is unchanged.
///
/// Services are analysed one after another, so a later service reads what an
/// earlier one settled; only described intents are recorded, never failures,
/// so a later service still asks for a function an earlier one failed on.
#[derive(Debug, Clone, Default)]
pub struct RunIntentMemo {
    described: std::sync::Arc<std::sync::Mutex<HashMap<(String, String), String>>>,
}

impl RunIntentMemo {
    fn get(&self, name: &str, hash: &str) -> Option<String> {
        self.described
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(name.to_string(), hash.to_string()))
            .cloned()
    }

    fn record(&self, name: &str, hash: &str, intent: &str) {
        self.described
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((name.to_string(), hash.to_string()))
            .or_insert_with(|| intent.to_string());
    }
}

/// Bodies at or under this size, on a single line, are trivial
/// single-expression helpers (getters, re-exports, `(x) => x.id`-style
/// lambdas). The function's name and signature — already in the index —
/// say everything an LLM sentence would add, so skipping the
/// `/generate-intent` call loses nothing while removing a large share of
/// call volume on real repos. Trivial functions keep `intent = None`;
/// callers simply get no context line for them (their bodies are equally
/// readable inline).
const TRIVIAL_BODY_MAX_CHARS: usize = 80;

/// A body too small to carry business logic worth an LLM description:
/// single-line and at most [`TRIVIAL_BODY_MAX_CHARS`] chars after trim.
/// Counted in chars, not bytes, so non-ASCII identifiers/strings don't
/// shrink the effective threshold.
fn is_trivial_body(body: &str) -> bool {
    let trimmed = body.trim();
    !trimmed.contains('\n') && trimmed.chars().count() <= TRIVIAL_BODY_MAX_CHARS
}

/// The local functions `def` calls, as definition keys.
///
/// Reads the call edges `crate::call_graph` resolved at discovery time (from
/// the AST, through the calling file's imports) rather than re-deriving
/// anything from body text. Text matching is what put `$`, `skeleton` and
/// `ask` in the callee list of a formatter whose body only mentions them
/// inside a template literal (#581).
///
/// A callee ref is a `(name, file_path)` locator, not a merged-map key: a
/// definition whose name another file also defines is stored under a re-keyed
/// row (#582), and its ref still names it plainly. So resolve through
/// [`definitions_by_location`] rather than by key, and return the map keys the
/// dependency ordering and intent context below are stated in.
///
/// An edge counts only when the map holds a row at that name AND that file.
/// Without the file check a caller would take its dependency ordering, and its
/// callee intent context, from an unrelated same-named function.
fn resolved_callees<'a>(
    def: &FunctionDefinition,
    by_location: &HashMap<(&'a str, &'a Path), &'a str>,
) -> Vec<String> {
    def.calls
        .iter()
        .filter_map(|call| {
            by_location
                .get(&(call.name.as_str(), Path::new(call.file_path.as_str())))
                .map(|key| (*key).to_string())
        })
        .collect()
}

/// Every definition indexed by where it is defined: `(name, file)` → map key.
/// The pair is unique — two rows sharing a name are in different files by
/// construction, which is exactly what the re-keying at merge time guarantees.
fn definitions_by_location(
    function_definitions: &HashMap<String, FunctionDefinition>,
) -> HashMap<(&str, &Path), &str> {
    function_definitions
        .iter()
        .map(|(key, def)| ((def.name.as_str(), def.file_path.as_path()), key.as_str()))
        .collect()
}

/// A cache-miss function awaiting its `/generate-intent` call: everything the
/// payload needs, plus the content hash to persist if the call succeeds.
struct Pending {
    name: String,
    /// Where the function is defined. Only orders a level into batches, so
    /// that neighbours in one file share a request.
    file_path: String,
    body: String,
    called_intents: Vec<String>,
    hash: String,
}

/// Most functions per `/generate-intent` request.
///
/// Equal to the lambda's `MAX_BATCH_FUNCTIONS` (carrick-cloud
/// `lambdas/generate-intent/batch_prompt.ts`), which is also the batch size
/// the intent-quality gate measured. The lambda refuses a larger batch with a
/// 400, so a size above it would only ever reach the single-call fallback.
const MAX_INTENT_BATCH: usize = 20;

/// Body plus helper-intent characters one batch may carry before the next
/// batch starts. Bodies are already cut to about 2,000 characters at
/// discovery, so this binds only on helper-heavy callers, and keeps one
/// request's prompt in the range the gate measured (a function over it goes
/// in a batch of its own).
const INTENT_BATCH_CHAR_BUDGET: usize = 60_000;

/// Functions per `/generate-intent` request: `CARRICK_INTENT_BATCH_SIZE`,
/// else [`MAX_INTENT_BATCH`], clamped to `1..=MAX_INTENT_BATCH`. `1` sends
/// every function on its own, exactly the request a scanner before
/// carrick#1064 sent.
fn intent_batch_size() -> usize {
    std::env::var("CARRICK_INTENT_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(MAX_INTENT_BATCH)
        .clamp(1, MAX_INTENT_BATCH)
}

fn pending_chars(pending: &Pending) -> usize {
    pending.body.len()
        + pending
            .called_intents
            .iter()
            .map(String::len)
            .sum::<usize>()
}

/// One level's cache misses cut into requests: ordered by file then name, so
/// a file's functions sit together and the cut is the same on every run, then
/// filled up to `size` functions or [`INTENT_BATCH_CHAR_BUDGET`] characters.
/// Names are map keys, unique by construction, so a batch never holds two
/// functions the answer could not tell apart.
fn batch_units(mut pending: Vec<Pending>, size: usize) -> Vec<Vec<Pending>> {
    pending.sort_by(|a, b| {
        (a.file_path.as_str(), a.name.as_str()).cmp(&(b.file_path.as_str(), b.name.as_str()))
    });
    let size = size.max(1);
    let mut units: Vec<Vec<Pending>> = Vec::new();
    let mut current: Vec<Pending> = Vec::new();
    let mut chars = 0usize;
    for item in pending {
        let item_chars = pending_chars(&item);
        if !current.is_empty()
            && (current.len() >= size || chars + item_chars > INTENT_BATCH_CHAR_BUDGET)
        {
            units.push(std::mem::take(&mut current));
            chars = 0;
        }
        chars += item_chars;
        current.push(item);
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

/// The single-function request body, unchanged since before batching.
fn single_payload(pending: &Pending) -> serde_json::Value {
    serde_json::json!({
        "name": pending.name,
        "body": pending.body,
        "called_intents": pending.called_intents,
    })
}

/// The batched request body (carrick#1064).
fn batch_payload(unit: &[Pending]) -> serde_json::Value {
    serde_json::json!({
        "functions": unit.iter().map(single_payload).collect::<Vec<_>>(),
    })
}

/// The batched answer: the lambda's `text` is `{"intents":[{name, intent, cached}]}`.
#[derive(serde::Deserialize)]
struct BatchAnswer {
    intents: Vec<BatchAnswerRow>,
}

#[derive(serde::Deserialize)]
struct BatchAnswerRow {
    name: String,
    #[serde(default)]
    intent: Option<String>,
}

/// The intents a batched answer gives, by name. A function is answered only
/// when its name comes back exactly once with a non-null intent; one that is
/// null, missing or repeated is left out, for the caller to send on its own.
/// `None` when the text is not a batched answer at all.
///
/// The acceptance gate (trimmed, non-empty, under 500 bytes) is not applied
/// here: an answered text goes through the same gate a single call's does.
fn read_batch_answer(text: &str, unit: &[Pending]) -> Option<HashMap<String, String>> {
    let answer: BatchAnswer = serde_json::from_str(text).ok()?;
    let requested: HashSet<&str> = unit.iter().map(|p| p.name.as_str()).collect();
    let mut seen: HashMap<String, Vec<Option<String>>> = HashMap::new();
    for row in answer.intents {
        if requested.contains(row.name.as_str()) {
            seen.entry(row.name).or_default().push(row.intent);
        }
    }
    Some(
        seen.into_iter()
            .filter_map(|(name, mut intents)| match intents.len() {
                1 => intents.pop().flatten().map(|intent| (name, intent)),
                _ => None,
            })
            .collect(),
    )
}

/// Whether a refused batch means the lambda does not take batches at all: a
/// lambda from before carrick#1064 reads `{functions}` as a single request
/// missing its `name`, and answers a non-retriable `validation_failed`.
fn batch_refused_as_unsupported(error: &AgentCallError) -> bool {
    error.code == "validation_failed" && !error.retriable
}

/// What one service's intent pass learned about batching while it ran.
struct BatchState {
    /// Cleared the first time the lambda refuses a batch as a request it does
    /// not understand; every later request in the pass is a single call.
    supported: std::sync::atomic::AtomicBool,
    /// Functions a batch left unanswered that were sent again on their own.
    singled: std::sync::atomic::AtomicUsize,
}

impl BatchState {
    fn new() -> Self {
        Self {
            supported: std::sync::atomic::AtomicBool::new(true),
            singled: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// Describe one request's worth of functions, each result paired with its
/// function in input order.
///
/// A batch whose answer does not cover a function (unreadable, cut, a name
/// missing or repeated, a null intent) sends that function again on its own,
/// so a mismatch costs a call and never an intent. A batch the lambda refused
/// as unsupported sends every function on its own and stops batching for the
/// rest of the pass. Any other failure (a spent retry chain, a budget refusal,
/// the quota breaker) is that failure for every function in the batch, exactly
/// as it would have been for each single call.
async fn describe_unit<S, SFut>(
    unit: Vec<Pending>,
    state: &BatchState,
    send: S,
) -> Vec<(Pending, Result<String, AgentCallError>)>
where
    S: Fn(serde_json::Value) -> SFut,
    SFut: std::future::Future<Output = Result<String, AgentCallError>>,
{
    use std::sync::atomic::Ordering;

    let batched = unit.len() > 1 && state.supported.load(Ordering::Relaxed);
    let answered: HashMap<String, String> = if !batched {
        HashMap::new()
    } else {
        match send(batch_payload(&unit)).await {
            Ok(text) => read_batch_answer(&text, &unit).unwrap_or_else(|| {
                debug!(
                    "Batched intent answer for {} function(s) was unreadable; sending each on its own",
                    unit.len()
                );
                HashMap::new()
            }),
            Err(error) if batch_refused_as_unsupported(&error) => {
                if state.supported.swap(false, Ordering::Relaxed) {
                    warn!(
                        "/generate-intent refused a batched request ({}); sending one function per request for the rest of this service",
                        error
                    );
                }
                HashMap::new()
            }
            Err(error) => {
                return unit
                    .into_iter()
                    .map(|pending| (pending, Err(error.clone())))
                    .collect();
            }
        }
    };

    let mut done: Vec<(usize, Pending, Result<String, AgentCallError>)> = Vec::new();
    let mut again: Vec<(usize, Pending)> = Vec::new();
    for (idx, pending) in unit.into_iter().enumerate() {
        match answered.get(&pending.name) {
            Some(intent) => {
                let intent = intent.clone();
                done.push((idx, pending, Ok(intent)));
            }
            None => again.push((idx, pending)),
        }
    }
    if batched && !again.is_empty() {
        state.singled.fetch_add(again.len(), Ordering::Relaxed);
    }
    let singles = futures::future::join_all(again.into_iter().map(|(idx, pending)| {
        let request = send(single_payload(&pending));
        async move { (idx, pending, request.await) }
    }))
    .await;
    done.extend(singles);
    done.sort_by_key(|(idx, _, _)| *idx);
    done.into_iter()
        .map(|(_, pending, result)| (pending, result))
        .collect()
}

/// Concurrent `/generate-intent` calls in flight per dependency level when
/// `CARRICK_INTENT_CONCURRENCY` is unset.
///
/// The same depth file analysis queues at. This was 8, on the reading that
/// intent calls, one per function, would push a higher request rate against
/// the backend quota (#460). A measured first index of a large monorepo said
/// otherwise: at 8 the stage ran well under what 8 slots could carry, and the
/// 429s it met tracked backend capacity, not the scanner's request rate, so
/// the low depth bought no fewer 429s and cost most of the scan's wall clock
/// (carrick#1065). The intent stage also runs beside file analysis now, and
/// both draw on one process-wide semaphore that bounds what is in flight.
const DEFAULT_INTENT_CONCURRENCY: usize = 20;

/// In-flight `/generate-intent` calls queued per level.
///
/// `CARRICK_INTENT_CONCURRENCY` overrides the default. It is a queue depth:
/// the process-wide semaphore behind `AgentService` (`CARRICK_CONCURRENCY_LIMIT`)
/// caps every lambda call in flight, and file analysis draws on the same one
/// while both stages run.
fn intent_concurrency() -> usize {
    std::env::var("CARRICK_INTENT_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_INTENT_CONCURRENCY)
        .max(1)
}

/// Dispatch one dependency level's requests at most `concurrency` at a time,
/// returning each request's output **in input order**. A request is one
/// `Pending` or one batch of them; the output carries each `Pending` with its
/// own result either way.
///
/// Completion order under `buffer_unordered` is arbitrary — a slow call
/// finishes after ones queued behind it. Outputs are therefore carried with
/// their input position and re-sorted by it, so neither the fold nor any
/// future caller can associate an intent with the wrong function, and a
/// re-run over the same level produces the same sequence regardless of
/// backend timing.
async fn generate_level<T, R, F, Fut>(units: Vec<T>, concurrency: usize, call: F) -> Vec<R>
where
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let mut results: Vec<(usize, R)> =
        futures::stream::iter(units.into_iter().enumerate().map(|(idx, item)| {
            let fut = call(item);
            async move { (idx, fut.await) }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;

    results.sort_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, output)| output).collect()
}

/// Generate intents for every function with a non-trivial body source,
/// regardless of export status. The only exclusion is a trivial body
/// (single line, at most [`TRIVIAL_BODY_MAX_CHARS`] chars), which keeps
/// `intent = None` and never costs a lambda call. Eligible functions
/// therefore include non-exported named declarations, const-bound
/// arrows/function expressions, and synthetic route/event callback handlers.
///
/// After generation:
/// - Each function's `intent` is populated with a 1-2 sentence description
/// - Each function's `intent_input_hash` records the content hash that produced it
/// - Each function's `calls` is populated with references to local callees
/// - `body_source` is stripped from ALL functions (source stays in GitHub, not AWS)
///
/// `previous` is what the previous scan of this service left (see
/// [`PreviousIntents`]): a function whose freshly computed hash it holds reuses
/// that intent without calling `/generate-intent`. Pass
/// `PreviousIntents::default()` when there is no previous scan. `memo` holds
/// what earlier services in this run described, so a function two members
/// share is asked for once.
///
/// A function none of whose callees failed is hashed and asked exactly as it
/// always was. A function with a callee that got no intent this run (a failed,
/// refused, aborted or discarded answer, or a callee itself deferred) is
/// deferred: it is not hashed, no request is sent for it, it keeps its previous
/// intent and hash when it has one, and it is asked on the next scan, when the
/// callee is. A caller is never keyed on a missing callee, so one failed call
/// does not re-key its callers on the scan after (carrick#1080 D7).
pub async fn generate_function_intents(
    agent_service: &AgentService,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    previous: &PreviousIntents,
    memo: &RunIntentMemo,
) {
    describe_functions(function_definitions, previous, memo, |payload| async move {
        // The mock seed only picks a canned answer in mock mode.
        let seed = payload
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("batch")
            .to_string();
        agent_service
            .post_to_lambda("/generate-intent", &payload, &seed)
            .await
    })
    .await
}

/// [`generate_function_intents`] over any `/generate-intent` transport: `send`
/// takes one request body and returns the lambda's answer text. The public
/// function passes the real lambda; tests pass a recorder.
async fn describe_functions<S, SFut>(
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    previous: &PreviousIntents,
    memo: &RunIntentMemo,
    send: S,
) where
    S: Fn(serde_json::Value) -> SFut,
    SFut: std::future::Future<Output = Result<String, AgentCallError>>,
{
    // Process every function with a body source, skipping trivial
    // single-line bodies (see TRIVIAL_BODY_MAX_CHARS): no lambda call,
    // no intent, permanently cheap. There is no export gate. Non-exported
    // functions, const-bound arrows, and synthetic callback handlers all
    // qualify.
    let eligible: Vec<String> = function_definitions
        .iter()
        .filter(|(_, def)| {
            def.body_source
                .as_ref()
                .is_some_and(|body| !is_trivial_body(body))
        })
        .map(|(name, _)| name.clone())
        .collect();

    if eligible.is_empty() {
        strip_body_source(function_definitions);
        return;
    }

    debug!("Generating intents for {} function(s)", eligible.len());

    // Dependency order comes from the call edges resolved at discovery
    // (`crate::call_graph`), which are already on each definition's `calls`.
    // Leaves first, so a caller's prompt carries its callees' intents.
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    {
        let by_location = definitions_by_location(function_definitions);
        for name in &eligible {
            if let Some(def) = function_definitions.get(name) {
                deps.insert(name.clone(), resolved_callees(def, &by_location));
            }
        }
    }

    // CARRICK_SKIP_INTENTS: stop before any /generate-intent lambda call.
    // Intents are one LLM call per eligible function — the dominant cost of
    // scanning a large repo — and feed only the MCP index; no cross-repo
    // analysis or eval dimension consumes them. Nothing deterministic is lost:
    // `calls` was resolved at discovery, before this function was reached, and
    // body_source is still stripped (source stays in GitHub, not AWS).
    //
    // A dispatched run skips them for a different reason and on the same line:
    // it is building prompts, not answering them, and it writes no index for an
    // intent to live in. The machine that resumes generates them
    // (carrick#1229).
    if std::env::var("CARRICK_SKIP_INTENTS").is_ok() || crate::analysis_channel::dispatching() {
        debug!(
            "Skipping intent generation for {} function(s)",
            eligible.len()
        );
        // Contract under the flag: NO intents at all — clear any pre-seeded
        // values so a caller can never upload stale ones.
        for def in function_definitions.values_mut() {
            def.intent = None;
            def.intent_input_hash = None;
        }
        strip_body_source(function_definitions);
        return;
    }

    // Topological sort into levels: functions at the same level can run in parallel
    let levels = topological_levels(&eligible, &deps);

    // Generate intents level by level — within each level, calls run in parallel.
    // Both the system instruction and user-prompt template live in the
    // /generate-intent lambda (carrick-cloud/lambdas/generate-intent/index.js).
    //
    // Caching is content-addressed: for each function we compute a hash over its
    // body and its callees' (already-resolved) intents. If that hash was seen in
    // the previous scan, we reuse the prior intent without a lambda call. This
    // both avoids redundant calls for unchanged code AND correctly invalidates a
    // caller when a callee's intent changed (its `called_intents` differ, so its
    // hash differs). Processing leaves-first guarantees callee intents are
    // resolved before their callers are hashed.
    //
    // `intents` holds the resolved intent per function (reused or freshly
    // generated); `hashes` holds the content hash that produced each one, to be
    // persisted on the definition for the next scan.
    let batch_size = intent_batch_size();
    let batch_state = BatchState::new();
    let mut intents: HashMap<String, String> = HashMap::new();
    let mut hashes: HashMap<String, String> = HashMap::new();
    let mut reused = 0usize;
    let mut shared = 0usize;
    let mut generated = 0usize;
    // Functions that ended their level without an intent: their own call
    // failed, or a callee's did. A caller of any of them is deferred rather
    // than keyed without it (carrick#1080 D7).
    let mut undescribed: HashSet<String> = HashSet::new();
    // The subset whose own request failed. Those get no intent this run, as a
    // failed call always gave; every other undescribed function keeps its
    // previous one.
    let mut failed_calls: HashSet<String> = HashSet::new();
    let mut deferred = 0usize;
    // Intents are one call per eligible function and the long phase of a
    // hosted scan, so this is the count a waiting parent renders
    // (carrick#955). A function is counted once it is settled, whether that
    // took a call, a cache hit or a deferral.
    let mut describing =
        crate::progress::Ticker::new(crate::progress::Phase::Intents, eligible.len());

    for (level_idx, level) in levels.iter().enumerate() {
        // Compute each function's called_intents context and content hash, then
        // split into cache hits (reuse) and misses (call the lambda).
        let mut to_generate: Vec<Pending> = Vec::new();
        // Merged into `undescribed` only after the level, so a deferral in the
        // cycle level never depends on the order that level is walked in.
        let mut deferred_here: Vec<String> = Vec::new();

        for name in level {
            let Some(def) = function_definitions.get(name) else {
                continue;
            };
            let Some(body) = def.body_source.as_ref() else {
                continue;
            };

            if deps
                .get(name)
                .is_some_and(|called| called.iter().any(|callee| undescribed.contains(callee)))
            {
                deferred_here.push(name.clone());
                describing.item();
                continue;
            }

            let called_intents: Vec<String> = deps
                .get(name)
                .map(|called| {
                    called
                        .iter()
                        .filter_map(|callee| {
                            intents
                                .get(callee)
                                .map(|intent| format!("- {}: {}", callee, intent))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let hash = compute_intent_hash(body, &called_intents);

            if let Some(prev_intent) = previous.for_hash(&hash) {
                // Identical body + callee context as a prior scan — reuse.
                memo.record(name, &hash, prev_intent);
                intents.insert(name.clone(), prev_intent.to_string());
                hashes.insert(name.clone(), hash);
                reused += 1;
                describing.item();
            } else if let Some(intent) = memo.get(name, &hash) {
                // The same request another service in this run already made.
                intents.insert(name.clone(), intent);
                hashes.insert(name.clone(), hash);
                shared += 1;
                describing.item();
            } else {
                to_generate.push(Pending {
                    name: name.clone(),
                    file_path: def.file_path.to_string_lossy().into_owned(),
                    body: body.clone(),
                    called_intents,
                    hash,
                });
            }
        }

        if !deferred_here.is_empty() {
            debug!(
                "Intent level {}/{}: deferred {} function(s) to the next scan, behind a callee that got no intent",
                level_idx + 1,
                levels.len(),
                deferred_here.len()
            );
        }
        deferred += deferred_here.len();
        undescribed.extend(deferred_here);

        if to_generate.is_empty() {
            continue;
        }

        // Run this level's cache-miss lambda calls with bounded concurrency
        // (#460). Every call in a level is independent, so the old unbounded
        // `join_all` queued the whole level at once; on a function-dense repo
        // that is thousands of simultaneous requests, and the backend answers
        // the overflow with a 429-wrapped 503 that costs those functions their
        // intents.
        //
        // Several functions share a request (carrick#1064): the level's misses
        // are cut into batches of up to `intent_batch_size()`, and the queue
        // depth now counts requests, not functions.
        let attempted = to_generate.len();
        let units = batch_units(to_generate, batch_size);
        let state = &batch_state;
        let send = &send;
        let outcomes: Vec<(Pending, Result<String, AgentCallError>)> =
            generate_level(units, intent_concurrency(), |unit| async move {
                describe_unit(unit, state, send).await
            })
            .await
            .into_iter()
            .flatten()
            .collect();

        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let mut aborted = 0usize;

        for (pending, result) in outcomes {
            describing.item();
            let Pending { name, hash, .. } = pending;
            match result {
                Ok(intent) => {
                    let intent = intent.trim().to_string();
                    if !intent.is_empty() && intent.len() < 500 {
                        memo.record(&name, &hash, &intent);
                        hashes.insert(name.clone(), hash);
                        intents.insert(name, intent);
                        generated += 1;
                        succeeded += 1;
                    } else {
                        // Empty or over-long response: drop it. The function
                        // keeps `intent = None` and its callers are deferred,
                        // so all of them are asked next scan. Log it —
                        // otherwise this is a silent, permanent cache miss.
                        warn!(
                            "Discarding intent for {} ({} chars, expected 1..500)",
                            name,
                            intent.len()
                        );
                        undescribed.insert(name.clone());
                        failed_calls.insert(name);
                        failed += 1;
                    }
                }
                Err(e) => {
                    // Degrade gracefully: no intent and no content hash is
                    // written, its callers are deferred, and the next scan
                    // asks exactly these and replays the rest from cache.
                    undescribed.insert(name.clone());
                    failed_calls.insert(name.clone());
                    if e.is_quota_abort() {
                        aborted += 1;
                    } else {
                        warn!("Failed to generate intent for {}: {}", name, e);
                        failed += 1;
                    }
                }
            }
        }

        // One line per level that actually called out, so a degraded scan is
        // visible as a number rather than as N scattered warnings.
        // Aborts are named only when they happened, but they must be named:
        // without them `attempted` would not equal succeeded + failed and the
        // line would read as unexplained loss.
        let summary = format!(
            "Intent level {}/{}: attempted {}, succeeded {}, failed after retry {}{}",
            level_idx + 1,
            levels.len(),
            attempted,
            succeeded,
            failed,
            if aborted > 0 {
                format!(", aborted on backend quota {}", aborted)
            } else {
                String::new()
            }
        );
        // Counted against the service being analysed, so the engine can give
        // it one more try before the run ends.
        crate::scan_health::record_intents_failed(failed);
        if failed > 0 || aborted > 0 {
            warn!("{}", summary);
        } else {
            debug!("{}", summary);
        }

        // The quota breaker is process-global and does not clear inside a
        // scan: every remaining call would fail instantly without reaching the
        // model, so stop here rather than logging a level's worth of aborts.
        if rate_limit_tripped() {
            warn!(
                "Backend LLM quota exhausted; stopping intent generation ({} call(s) in this level aborted unattempted)",
                aborted
            );
            break;
        }
    }

    // Every eligible function that is not described and whose own call did not
    // fail was deferred: behind a failed callee, or unreached because the quota
    // breaker stopped the pass. It keeps what the previous scan gave it, with
    // the hash that produced it, so the next scan reuses it only if its inputs
    // come out the same once its callees are described.
    let mut kept = 0usize;
    for name in &eligible {
        if intents.contains_key(name) || failed_calls.contains(name) {
            continue;
        }
        let Some(def) = function_definitions.get_mut(name) else {
            continue;
        };
        if let Some(prev) = previous.for_function(name) {
            def.intent = Some(prev.intent.clone());
            def.intent_input_hash = prev.hash.clone();
            kept += 1;
        }
    }
    if deferred > 0 {
        warn!(
            "Deferred {} function(s) to the next scan because a function they call got no intent; {} kept their previous intent",
            deferred, kept
        );
    }

    // Write resolved intents and their content hashes back to the definitions.
    let total = intents.len();
    for (name, intent) in intents {
        if let Some(def) = function_definitions.get_mut(&name) {
            def.intent = Some(intent);
            def.intent_input_hash = hashes.get(&name).cloned();
        }
    }

    debug!(
        "Intents: {} total ({} reused from content-hash cache, {} described earlier in this run, {} freshly generated; batch size {}, {} sent again on their own after a batch left them unanswered)",
        total,
        reused,
        shared,
        generated,
        batch_size,
        batch_state
            .singled
            .load(std::sync::atomic::Ordering::Relaxed)
    );

    // Strip body_source — source code stays in GitHub, not AWS
    strip_body_source(function_definitions);
}

/// [`generate_function_intents`] started on a task of its own, so a service's
/// intents are generated while the rest of its analysis runs (carrick#1065).
///
/// Intents need only what discovery produced (the definitions, with their
/// bodies and resolved call edges) and the previous scan's hashes; nothing the
/// file analyzer, the mount graph or the protocol scans compute feeds them.
/// Waiting for the file analyzer's last file before asking for the first
/// intent left the intent stage idle for the whole of the model stage.
///
/// Dropping the handle aborts the task. A service whose analysis fails part
/// way returns early past the point that would have collected the intents,
/// and the task must not go on calling the model for a result nobody reads.
pub struct IntentsInFlight {
    task: AbortOnDrop<HashMap<String, FunctionDefinition>>,
}

impl IntentsInFlight {
    /// Start generating intents for `function_definitions` on the runtime.
    pub fn start(
        agent_service: AgentService,
        mut function_definitions: HashMap<String, FunctionDefinition>,
        previous: PreviousIntents,
        memo: RunIntentMemo,
    ) -> Self {
        let task = tokio::spawn(async move {
            generate_function_intents(&agent_service, &mut function_definitions, &previous, &memo)
                .await;
            function_definitions
        });
        Self {
            task: AbortOnDrop(Some(task)),
        }
    }

    /// Wait for the intents and take the definitions back, intents written and
    /// `body_source` stripped, exactly as [`generate_function_intents`] leaves
    /// them.
    pub async fn finish(self) -> HashMap<String, FunctionDefinition> {
        self.task.join().await
    }
}

/// A spawned task that is aborted when its handle is dropped before it has
/// been joined.
struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    /// Wait for the task's value. A panic inside the task resumes here, as it
    /// would have had the work run inline.
    async fn join(mut self) -> T {
        let handle = self
            .0
            .as_mut()
            .expect("an AbortOnDrop is joined at most once");
        // Awaited through `&mut` so that the handle stays in `self` while it
        // is pending: if THIS future is dropped mid-wait, `Drop` still aborts.
        let joined = handle.await;
        self.0 = None;
        match joined {
            Ok(value) => value,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => panic!("task ended without a result: {error}"),
        }
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// Remove body_source from all function definitions.
/// The intent is the index; GitHub is the source of truth for code.
fn strip_body_source(function_definitions: &mut HashMap<String, FunctionDefinition>) {
    for def in function_definitions.values_mut() {
        def.body_source = None;
    }
}

/// Topological sort into parallel levels.
/// Level 0 = functions with no local deps (leaves).
/// Level 1 = functions whose deps are all in level 0. Etc.
/// Functions within the same level can run in parallel.
fn topological_levels(names: &[String], deps: &HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    let name_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut reverse_deps: HashMap<&str, Vec<&str>> = HashMap::new();

    for name in names {
        in_degree.entry(name.as_str()).or_insert(0);
        if let Some(called) = deps.get(name) {
            for callee in called {
                if name_set.contains(callee.as_str()) {
                    *in_degree.entry(name.as_str()).or_insert(0) += 1;
                    reverse_deps
                        .entry(callee.as_str())
                        .or_default()
                        .push(name.as_str());
                }
            }
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<&str> = in_degree
        .iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    while !current.is_empty() {
        levels.push(current.iter().map(|s| s.to_string()).collect());
        let mut next = Vec::new();
        for &name in &current {
            if let Some(dependents) = reverse_deps.get(name) {
                for &dep in dependents {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            next.push(dep);
                        }
                    }
                }
            }
        }
        current = next;
    }

    // Add any remaining (cycles) as a final level
    let in_levels: HashSet<&str> = levels.iter().flatten().map(|s| s.as_str()).collect();
    let remaining: Vec<String> = names
        .iter()
        .filter(|n| !in_levels.contains(n.as_str()))
        .cloned()
        .collect();
    if !remaining.is_empty() {
        levels.push(remaining);
    }

    levels
}

// build_intent_prompt was moved to carrick-cloud/lambdas/generate-intent/index.js
// (buildPrompt). Rust now sends {name, body, called_intents} as a structured
// payload; the lambda assembles the prompt from those fields.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visitor::FunctionCallRef;

    /// Dependency order comes from the resolved call edges, not from body
    /// text. `processId` contains "id" as a substring and `id`'s body names
    /// `processId` in a comment; under the old text matcher that pair formed
    /// a fake cycle that dumped both functions into the unordered cycle level
    /// (#55, #141, #581).
    #[test]
    fn deps_come_from_resolved_edges_not_body_text() {
        let names = vec!["id".to_string(), "processId".to_string()];
        let mut defs = HashMap::new();
        defs.insert(
            "id".to_string(),
            def_with_body("id", "// processId calls this\nreturn 1;"),
        );
        defs.insert(
            "processId".to_string(),
            with_calls(
                def_with_body("processId", "const n = id();\nreturn n;"),
                vec![call_ref("id", "test.ts", 1, 1)],
            ),
        );

        let mut deps = HashMap::new();
        for name in &names {
            deps.insert(
                name.clone(),
                resolved_callees(&defs[name], &definitions_by_location(&defs)),
            );
        }

        assert_eq!(deps["id"], Vec::<String>::new());
        assert_eq!(deps["processId"], vec!["id".to_string()]);

        let levels = topological_levels(&names, &deps);
        assert_eq!(levels.len(), 2, "leaf level then caller level, no cycle");
        assert_eq!(levels[0], vec!["id".to_string()]);
        assert_eq!(levels[1], vec!["processId".to_string()]);
    }

    /// An edge counts as a dependency only when the map holds a row with that
    /// name AND that file. An edge into a file that was not indexed, or that
    /// has no row at all, is dropped rather than folded into the caller's
    /// intent context.
    #[test]
    fn resolved_callees_require_a_matching_row_and_file() {
        let mut defs = HashMap::new();
        defs.insert("helper".to_string(), def_with_body("helper", "return 1;"));
        defs.insert(
            "main".to_string(),
            with_calls(
                def_with_body("main", "return helper();"),
                vec![
                    call_ref("helper", "test.ts", 1, 1),
                    // Same key, another file: the row we hold is not this one.
                    call_ref("helper", "other.ts", 4, 2),
                    // No row at all.
                    call_ref("vanished", "gone.ts", 7, 3),
                ],
            ),
        );

        assert_eq!(
            resolved_callees(&defs["main"], &definitions_by_location(&defs)),
            vec!["helper".to_string()]
        );
    }

    /// A callee whose name another file also defines is stored under a re-keyed
    /// row (#582) while its ref still names it plainly. The dependency has to
    /// come back as the MAP KEY, because that is what the level ordering and
    /// the `intents` map below are keyed by — a plain name would silently drop
    /// the callee's intent out of its caller's prompt.
    #[test]
    fn resolved_callees_return_the_rekeyed_map_key() {
        let mut defs = HashMap::new();
        let mut here = def_with_body("helper", "return 1;");
        here.file_path = "a.ts".into();
        let mut there = def_with_body("helper", "return 2;");
        there.file_path = "b.ts".into();
        defs.insert("helper@a.ts".to_string(), here);
        defs.insert("helper@b.ts".to_string(), there);
        defs.insert(
            "main".to_string(),
            with_calls(
                def_with_body("main", "return helper();"),
                vec![call_ref("helper", "b.ts", 1, 1)],
            ),
        );

        assert_eq!(
            resolved_callees(&defs["main"], &definitions_by_location(&defs)),
            vec!["helper@b.ts".to_string()],
            "the edge names the file it resolved to, so it must pick that row"
        );
    }

    #[test]
    fn topological_levels_leaves_first() {
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut deps = HashMap::new();
        // c calls a and b, b calls a
        deps.insert("c".to_string(), vec!["a".to_string(), "b".to_string()]);
        deps.insert("b".to_string(), vec!["a".to_string()]);

        let levels = topological_levels(&names, &deps);
        assert!(levels.len() >= 2, "should have at least 2 levels");
        // Level 0 should contain "a" (leaf)
        assert!(
            levels[0].contains(&"a".to_string()),
            "a should be in level 0"
        );
        // "c" should be in a later level than "b"
        let b_level = levels
            .iter()
            .position(|l| l.contains(&"b".to_string()))
            .unwrap();
        let c_level = levels
            .iter()
            .position(|l| l.contains(&"c".to_string()))
            .unwrap();
        assert!(b_level < c_level, "b should be in an earlier level than c");
    }

    #[test]
    fn topological_levels_no_deps_single_level() {
        let names = vec!["x".to_string(), "y".to_string()];
        let deps = HashMap::new();
        let levels = topological_levels(&names, &deps);
        assert_eq!(levels.len(), 1, "all functions should be in one level");
        assert_eq!(levels[0].len(), 2);
    }

    #[test]
    fn topological_levels_handles_cycles() {
        let names = vec!["a".to_string(), "b".to_string()];
        let mut deps = HashMap::new();
        deps.insert("a".to_string(), vec!["b".to_string()]);
        deps.insert("b".to_string(), vec!["a".to_string()]);
        let levels = topological_levels(&names, &deps);
        let total: usize = levels.iter().map(|l| l.len()).sum();
        assert_eq!(total, 2, "both should still appear");
    }

    // build_prompt_without_deps and build_prompt_with_deps were removed:
    // prompt construction moved to /generate-intent lambda. Equivalent
    // behavioural test now lives in carrick-cloud (TBD).

    #[test]
    fn strip_body_source_removes_all() {
        let mut defs = HashMap::new();
        defs.insert(
            "foo".to_string(),
            FunctionDefinition {
                name: "foo".to_string(),
                file_path: "test.ts".into(),
                node_type: Default::default(),
                arguments: vec![],
                body_source: Some("return 1;".to_string()),
                is_exported: true,
                line_number: 1,
                end_line: 0,
                intent: Some("returns one".to_string()),
                calls: vec![],
                tokens: vec!["budgetBytes".to_string(), "1500".to_string()],
                return_type: None,
                return_is_explicit: false,
                signature: None,
                intent_input_hash: None,
                dispatch_table: None,
            },
        );
        strip_body_source(&mut defs);
        assert!(defs.get("foo").unwrap().body_source.is_none());
        // Intent should be preserved
        assert!(defs.get("foo").unwrap().intent.is_some());
        // So must the retrieval tokens (carrick-cloud#434). They are collected
        // during extraction, not from `body_source`, and they are the only
        // thing left carrying the body's words once the source goes.
        assert_eq!(
            defs.get("foo").unwrap().tokens,
            vec!["budgetBytes".to_string(), "1500".to_string()]
        );
    }

    #[test]
    fn intent_hash_is_deterministic() {
        let called = vec!["- a: does a".to_string(), "- b: does b".to_string()];
        let h1 = compute_intent_hash("return 1;", &called);
        let h2 = compute_intent_hash("return 1;", &called);
        assert_eq!(h1, h2);
    }

    #[test]
    fn intent_hash_ignores_called_intents_order() {
        let a = vec!["- a: does a".to_string(), "- b: does b".to_string()];
        let b = vec!["- b: does b".to_string(), "- a: does a".to_string()];
        assert_eq!(
            compute_intent_hash("return 1;", &a),
            compute_intent_hash("return 1;", &b),
            "reordered callee intents must hash identically"
        );
    }

    #[test]
    fn intent_hash_changes_with_body() {
        let called: Vec<String> = vec![];
        assert_ne!(
            compute_intent_hash("return 1;", &called),
            compute_intent_hash("return 2;", &called)
        );
    }

    #[test]
    fn intent_hash_changes_when_callee_intent_changes() {
        // A caller whose callee's intent shifts must get a new hash so the
        // stale cached intent is regenerated rather than reused.
        let before = vec!["- helper: validates the token".to_string()];
        let after = vec!["- helper: parses the token".to_string()];
        assert_ne!(
            compute_intent_hash("return helper();", &before),
            compute_intent_hash("return helper();", &after)
        );
    }

    #[test]
    fn intents_by_hash_keeps_only_complete_entries() {
        let mut defs = HashMap::new();
        let base = FunctionDefinition {
            name: "f".to_string(),
            file_path: "test.ts".into(),
            node_type: Default::default(),
            arguments: vec![],
            body_source: None,
            is_exported: true,
            line_number: 1,
            end_line: 0,
            intent: None,
            calls: vec![],
            tokens: vec![],
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
            dispatch_table: None,
        };

        // Complete: both intent and hash present → kept.
        defs.insert(
            "complete".to_string(),
            FunctionDefinition {
                intent: Some("does the thing".to_string()),
                intent_input_hash: Some("abc123".to_string()),
                ..base.clone()
            },
        );
        // Intent but no hash (pre-content-hash scan) → skipped.
        defs.insert(
            "no_hash".to_string(),
            FunctionDefinition {
                intent: Some("does another thing".to_string()),
                ..base.clone()
            },
        );
        // Hash but no intent (generation failed) → skipped.
        defs.insert(
            "no_intent".to_string(),
            FunctionDefinition {
                intent_input_hash: Some("def456".to_string()),
                ..base.clone()
            },
        );

        let previous = PreviousIntents::from_definitions(&defs);
        assert_eq!(previous.by_hash.len(), 1);
        assert_eq!(previous.for_hash("abc123"), Some("does the thing"));

        // By function, an intent is kept with or without its hash.
        assert_eq!(previous.by_key.len(), 2);
        let complete = previous.for_function("complete").unwrap();
        assert_eq!(complete.hash.as_deref(), Some("abc123"));
        let no_hash = previous.for_function("no_hash").unwrap();
        assert_eq!(no_hash.intent, "does another thing");
        assert!(no_hash.hash.is_none());
        assert!(previous.for_function("no_intent").is_none());
    }

    /// Env vars are process-global and tests run in parallel: every test in
    /// THIS module that sets a CARRICK_* flag — or calls
    /// generate_function_intents while another of them could have one set —
    /// serializes on this lock (it is module-private, not a crate-wide
    /// guarantee). Tokio's mutex, so the guard may be held across await
    /// points.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A previous scan that holds only these content hashes.
    fn previous_by_hash(by_hash: HashMap<String, String>) -> PreviousIntents {
        PreviousIntents {
            by_hash,
            by_key: HashMap::new(),
        }
    }

    fn call_ref(name: &str, file: &str, line: u32, call_site: u32) -> FunctionCallRef {
        FunctionCallRef {
            name: name.to_string(),
            file_path: file.to_string(),
            line_number: line,
            call_site_line: call_site,
            call_count: 1,
        }
    }

    fn with_calls(def: FunctionDefinition, calls: Vec<FunctionCallRef>) -> FunctionDefinition {
        FunctionDefinition { calls, ..def }
    }

    fn def_with_body(name: &str, body: &str) -> FunctionDefinition {
        FunctionDefinition {
            name: name.to_string(),
            file_path: "test.ts".into(),
            node_type: Default::default(),
            arguments: vec![],
            body_source: Some(body.to_string()),
            is_exported: true,
            line_number: 1,
            end_line: 0,
            intent: None,
            calls: vec![],
            tokens: vec![],
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
            dispatch_table: None,
        }
    }

    /// When every function's content hash is present in the previous-scan map,
    /// all intents are reused and NO `/generate-intent` call is made (the test
    /// would otherwise hit the network and fail). Also exercises the caller's
    /// hash composing its callee's resolved intent. Bodies are multi-line so
    /// they clear the trivial-body gate.
    #[tokio::test]
    async fn full_cache_hit_makes_no_lambda_calls() {
        let _env = ENV_LOCK.lock().await;
        // `main` calls `helper`; helper is the leaf (level 0).
        let helper_body = "const rate = table[region];\nreturn base * rate;";
        let main_body = "const base = order.subtotal;\nreturn helper(base);";
        let mut defs = HashMap::new();
        defs.insert("helper".to_string(), def_with_body("helper", helper_body));
        defs.insert(
            "main".to_string(),
            with_calls(
                def_with_body("main", main_body),
                vec![call_ref("helper", "test.ts", 1, 2)],
            ),
        );

        // Reconstruct the exact hashes the generator will compute.
        let helper_intent = "applies the regional rate to a base amount";
        let helper_hash = compute_intent_hash(helper_body, &[]);
        let caller_context = vec![format!("- helper: {}", helper_intent)];
        let main_hash = compute_intent_hash(main_body, &caller_context);

        let mut prev = HashMap::new();
        prev.insert(helper_hash.clone(), helper_intent.to_string());
        prev.insert(main_hash.clone(), "calls the helper".to_string());

        let agent = AgentService::new();
        generate_function_intents(
            &agent,
            &mut defs,
            &previous_by_hash(prev),
            &RunIntentMemo::default(),
        )
        .await;

        // Both intents came from the cache, with their hashes recorded.
        assert_eq!(defs["helper"].intent.as_deref(), Some(helper_intent));
        assert_eq!(defs["main"].intent.as_deref(), Some("calls the helper"));
        assert_eq!(
            defs["helper"].intent_input_hash.as_deref(),
            Some(helper_hash.as_str())
        );
        assert_eq!(
            defs["main"].intent_input_hash.as_deref(),
            Some(main_hash.as_str())
        );
        // body_source is stripped before upload.
        assert!(defs["helper"].body_source.is_none());
        assert!(defs["main"].body_source.is_none());
    }

    #[test]
    fn trivial_body_gate() {
        // Single-expression one-liners: skipped.
        assert!(is_trivial_body("return 1;"));
        assert!(is_trivial_body("(x) => x.id"));
        assert!(is_trivial_body("{ return user.email; }"));
        assert!(is_trivial_body("  return config.baseUrl;  "));
        // Threshold counts chars, not bytes: a one-liner of 80 multi-byte
        // chars (240 bytes here) is still trivial.
        assert!(is_trivial_body(&"é".repeat(80)));
        assert!(!is_trivial_body(&"é".repeat(81)));

        // Multi-line bodies always get an intent, however short.
        assert!(!is_trivial_body("const a = 1;\nreturn a;"));
        // Long one-liners can still carry real logic.
        assert!(!is_trivial_body(
            "return users.filter((u) => u.active && !u.deleted && u.verifiedAt != null).map((u) => u.email);"
        ));
    }

    /// Trivial functions are excluded from generation entirely: no lambda
    /// call is attempted (the test would hit the network and fail if one
    /// were), no intent is recorded, and body_source is still stripped.
    #[tokio::test]
    async fn trivial_functions_are_skipped_without_lambda_calls() {
        let _env = ENV_LOCK.lock().await;
        let mut defs = HashMap::new();
        defs.insert("getId".to_string(), def_with_body("getId", "return x.id;"));

        let agent = AgentService::new();
        generate_function_intents(
            &agent,
            &mut defs,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
        )
        .await;

        assert!(defs["getId"].intent.is_none());
        assert!(defs["getId"].intent_input_hash.is_none());
        assert!(defs["getId"].body_source.is_none());
    }

    /// CARRICK_SKIP_INTENTS stops intent generation before any lambda call
    /// while keeping the deterministic parts: the `calls` edges resolved at
    /// discovery survive and body_source is stripped. Both cases run inside one test (sequentially)
    /// because env vars are process-global. Under CARRICK_MOCK_ALL the lambda
    /// path returns a mock intent, so pre-fix the skip case would record
    /// `Some("Mock intent: …")` and fail the `None` assertions.
    #[tokio::test]
    async fn skip_intents_flag_skips_lambda_calls_but_strips_bodies() {
        let _env = ENV_LOCK.lock().await;
        let helper_body = "const rate = table[region];\nreturn base * rate;";
        let main_body = "const base = order.subtotal;\nreturn helper(base);";
        let make_defs = || {
            let mut defs = HashMap::new();
            defs.insert("helper".to_string(), def_with_body("helper", helper_body));
            // Call edges are resolved at discovery (crate::call_graph), so a
            // definition reaching the generator already carries them.
            defs.insert(
                "main".to_string(),
                with_calls(
                    def_with_body("main", main_body),
                    vec![call_ref("helper", "test.ts", 1, 2)],
                ),
            );
            defs
        };
        let agent = AgentService::new();

        // Snapshot pre-existing values so a developer/CI environment that
        // already sets these flags is restored, not clobbered.
        let prev_mock = std::env::var("CARRICK_MOCK_ALL").ok();
        let prev_skip = std::env::var("CARRICK_SKIP_INTENTS").ok();

        // SAFETY: env vars are process-global; ENV_LOCK serializes this
        // module's env-touching tests, and no test outside it reads these
        // vars mid-flight (the network-averse tests above assert cache/skip
        // behavior that MOCK_ALL does not alter).
        unsafe {
            std::env::set_var("CARRICK_MOCK_ALL", "1");
            std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        }
        let mut defs = make_defs();
        generate_function_intents(
            &agent,
            &mut defs,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
        )
        .await;
        unsafe {
            std::env::remove_var("CARRICK_SKIP_INTENTS");
        }

        // No intents, no hashes — the lambda path never ran.
        assert!(defs["helper"].intent.is_none());
        assert!(defs["main"].intent.is_none());
        assert!(defs["helper"].intent_input_hash.is_none());
        // Deterministic outputs are intact: the caller→callee edge survives
        // the early exit, and bodies are still stripped.
        assert_eq!(defs["main"].calls.len(), 1);
        assert_eq!(defs["main"].calls[0].name, "helper");
        assert_eq!(defs["main"].calls[0].call_site_line, 2);
        assert!(defs["helper"].body_source.is_none());
        assert!(defs["main"].body_source.is_none());

        // Control: with the flag unset (MOCK_ALL still on), intents flow.
        let mut defs = make_defs();
        generate_function_intents(
            &agent,
            &mut defs,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
        )
        .await;

        // Restore whatever the environment had before the test.
        unsafe {
            match prev_mock {
                Some(v) => std::env::set_var("CARRICK_MOCK_ALL", v),
                None => std::env::remove_var("CARRICK_MOCK_ALL"),
            }
            match prev_skip {
                Some(v) => std::env::set_var("CARRICK_SKIP_INTENTS", v),
                None => std::env::remove_var("CARRICK_SKIP_INTENTS"),
            }
        }
        assert_eq!(
            defs["helper"].intent.as_deref(),
            Some("Mock intent: function does something.")
        );
        assert!(defs["main"].intent_input_hash.is_some());
    }

    fn pending(name: &str) -> Pending {
        Pending {
            name: name.to_string(),
            file_path: "test.ts".to_string(),
            body: format!("return {}();", name),
            called_intents: vec![],
            hash: format!("hash-of-{}", name),
        }
    }

    // ------------------------------------------------ batching (#1064)

    fn pending_in(name: &str, file: &str) -> Pending {
        Pending {
            file_path: file.to_string(),
            ..pending(name)
        }
    }

    /// A recorded `/generate-intent` transport: every payload it was sent, and
    /// an answer chosen by the test from that payload.
    struct Transport {
        sent: std::sync::Mutex<Vec<serde_json::Value>>,
    }

    impl Transport {
        fn new() -> Self {
            Self {
                sent: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn batches(&self) -> usize {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.get("functions").is_some())
                .count()
        }
        fn singles(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        }
    }

    fn batch_names(payload: &serde_json::Value) -> Vec<String> {
        payload["functions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn answer(rows: &[(&str, Option<&str>)]) -> String {
        serde_json::json!({
            "intents": rows
                .iter()
                .map(|(name, intent)| serde_json::json!({"name": name, "intent": intent, "cached": false}))
                .collect::<Vec<_>>()
        })
        .to_string()
    }

    fn single_answer(payload: &serde_json::Value) -> Result<String, AgentCallError> {
        Ok(format!("single for {}", payload["name"].as_str().unwrap()))
    }

    #[test]
    fn a_level_is_cut_by_file_then_name_and_by_size() {
        let level = vec![
            pending_in("z", "b.ts"),
            pending_in("b", "a.ts"),
            pending_in("a", "b.ts"),
            pending_in("a", "a.ts"),
            pending_in("c", "a.ts"),
        ];
        let units = batch_units(level, 2);
        let names: Vec<Vec<(String, String)>> = units
            .iter()
            .map(|u| {
                u.iter()
                    .map(|p| (p.file_path.clone(), p.name.clone()))
                    .collect()
            })
            .collect();
        let pair = |f: &str, n: &str| (f.to_string(), n.to_string());
        assert_eq!(
            names,
            vec![
                vec![pair("a.ts", "a"), pair("a.ts", "b")],
                vec![pair("a.ts", "c"), pair("b.ts", "a")],
                vec![pair("b.ts", "z")],
            ]
        );
    }

    #[test]
    fn a_batch_closes_at_the_character_budget() {
        let mut big = pending("big");
        big.body = "x".repeat(INTENT_BATCH_CHAR_BUDGET);
        let mut helpers = pending("helpers");
        helpers.called_intents = vec!["y".repeat(INTENT_BATCH_CHAR_BUDGET / 2); 3];
        let level = vec![pending("a"), big, pending("c"), helpers, pending("e")];
        let sizes: Vec<Vec<String>> = batch_units(level, 20)
            .iter()
            .map(|u| u.iter().map(|p| p.name.clone()).collect())
            .collect();
        // Sorted by name: a, big, c, e, helpers. `big` alone fills a batch, and
        // `helpers` is over the budget on its own, so it gets a batch too.
        assert_eq!(
            sizes,
            vec![
                vec!["a".to_string()],
                vec!["big".to_string()],
                vec!["c".to_string(), "e".to_string()],
                vec!["helpers".to_string()],
            ]
        );
    }

    #[tokio::test]
    async fn the_batch_size_knob_is_clamped_to_what_the_lambda_takes() {
        let _env = ENV_LOCK.lock().await;
        let prev = std::env::var("CARRICK_INTENT_BATCH_SIZE").ok();
        // SAFETY: serialized on ENV_LOCK and restored below.
        unsafe { std::env::remove_var("CARRICK_INTENT_BATCH_SIZE") };
        assert_eq!(intent_batch_size(), MAX_INTENT_BATCH);
        unsafe { std::env::set_var("CARRICK_INTENT_BATCH_SIZE", "1") };
        assert_eq!(intent_batch_size(), 1);
        unsafe { std::env::set_var("CARRICK_INTENT_BATCH_SIZE", "0") };
        assert_eq!(intent_batch_size(), 1);
        unsafe { std::env::set_var("CARRICK_INTENT_BATCH_SIZE", "500") };
        assert_eq!(intent_batch_size(), MAX_INTENT_BATCH);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CARRICK_INTENT_BATCH_SIZE", v),
                None => std::env::remove_var("CARRICK_INTENT_BATCH_SIZE"),
            }
        }
    }

    #[test]
    fn the_single_request_body_is_what_it_was_before_batching() {
        let mut p = pending("f");
        p.called_intents = vec!["- g: does g".to_string()];
        assert_eq!(
            single_payload(&p),
            serde_json::json!({"name": "f", "body": "return f();", "called_intents": ["- g: does g"]})
        );
        assert_eq!(
            batch_payload(&[pending("a"), pending("b")]),
            serde_json::json!({"functions": [
                {"name": "a", "body": "return a();", "called_intents": []},
                {"name": "b", "body": "return b();", "called_intents": []},
            ]})
        );
    }

    #[tokio::test]
    async fn a_full_batched_answer_takes_one_request() {
        let transport = Transport::new();
        let state = BatchState::new();
        let unit = vec![pending("a"), pending("b"), pending("c")];
        let out = describe_unit(unit, &state, |payload| {
            transport.sent.lock().unwrap().push(payload.clone());
            async move {
                let names = batch_names(&payload);
                let rows: Vec<(&str, Option<&str>)> = names
                    .iter()
                    .map(|n| (n.as_str(), Some("batched")))
                    .collect();
                Ok(answer(&rows))
            }
        })
        .await;
        assert_eq!(transport.batches(), 1);
        assert!(transport.singles().is_empty());
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|(p, r)| (p.name.as_str(), r.as_ref().unwrap().as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("a", "batched"), ("b", "batched"), ("c", "batched")]
        );
    }

    /// The orchestrating rule: a batch mismatch never costs an intent. Only the
    /// functions the answer did not cover are sent again, one at a time.
    #[tokio::test]
    async fn functions_a_batch_left_unanswered_are_sent_again_on_their_own() {
        let transport = Transport::new();
        let state = BatchState::new();
        let unit = vec![
            pending("ok"),
            pending("null"),
            pending("missing"),
            pending("twice"),
        ];
        let out = describe_unit(unit, &state, |payload| {
            transport.sent.lock().unwrap().push(payload.clone());
            async move {
                if payload.get("functions").is_some() {
                    Ok(answer(&[
                        ("ok", Some("fine")),
                        ("null", None),
                        ("twice", Some("one")),
                        ("twice", Some("two")),
                        ("stranger", Some("not asked")),
                    ]))
                } else {
                    single_answer(&payload)
                }
            }
        })
        .await;
        let mut singles = transport.singles();
        singles.sort();
        assert_eq!(singles, vec!["missing", "null", "twice"]);
        assert_eq!(state.singled.load(std::sync::atomic::Ordering::Relaxed), 3);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|(p, r)| (p.name.as_str(), r.as_ref().unwrap().as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("ok", "fine"),
                ("null", "single for null"),
                ("missing", "single for missing"),
                ("twice", "single for twice"),
            ],
            "input order, each result with its own function"
        );
    }

    #[tokio::test]
    async fn an_unreadable_batched_answer_sends_every_function_on_its_own() {
        for text in [
            "not json",
            r#"{"intents":[{"name":"a","intent":"cut"#,
            r#"[{"id":1,"intent":"x"}]"#,
        ] {
            let transport = Transport::new();
            let state = BatchState::new();
            let out = describe_unit(vec![pending("a"), pending("b")], &state, |payload| {
                transport.sent.lock().unwrap().push(payload.clone());
                let text = text.to_string();
                async move {
                    if payload.get("functions").is_some() {
                        Ok(text)
                    } else {
                        single_answer(&payload)
                    }
                }
            })
            .await;
            assert_eq!(transport.singles().len(), 2, "{text}");
            assert!(out.iter().all(|(_, r)| r.is_ok()), "{text}");
            // An unreadable answer is not a refusal: batching stays on.
            assert!(state.supported.load(std::sync::atomic::Ordering::Relaxed));
        }
    }

    /// A lambda from before #1064 reads `{functions}` as a single request with
    /// no name. The first refusal turns batching off for the rest of the pass,
    /// so an old deploy costs one refused request, not one per batch.
    #[tokio::test]
    async fn a_lambda_that_does_not_take_batches_turns_batching_off_for_the_pass() {
        let transport = Transport::new();
        let state = BatchState::new();
        let send = |payload: serde_json::Value| {
            transport.sent.lock().unwrap().push(payload.clone());
            async move {
                if payload.get("functions").is_some() {
                    Err(AgentCallError {
                        code: "validation_failed".to_string(),
                        message: "name (non-empty string) is required".to_string(),
                        retriable: false,
                    })
                } else {
                    single_answer(&payload)
                }
            }
        };
        let first = describe_unit(vec![pending("a"), pending("b")], &state, &send).await;
        let second = describe_unit(vec![pending("c"), pending("d")], &state, &send).await;
        assert_eq!(
            transport.batches(),
            1,
            "the second unit must not try a batch"
        );
        assert_eq!(transport.singles().len(), 4);
        assert!(first.iter().chain(second.iter()).all(|(_, r)| r.is_ok()));
    }

    /// A batch that failed the way a single call fails (retry chain spent,
    /// budget refusal, quota breaker) fails every function in it, and is not
    /// re-sent as singles that would meet the same wall.
    #[tokio::test]
    async fn a_failed_batch_fails_its_functions_without_single_calls() {
        for (code, retriable) in [
            ("model_error", true),
            ("llm_disabled", false),
            (crate::agent_service::QUOTA_ABORT_CODE, false),
        ] {
            let transport = Transport::new();
            let state = BatchState::new();
            let out = describe_unit(vec![pending("a"), pending("b")], &state, |payload| {
                transport.sent.lock().unwrap().push(payload.clone());
                async move {
                    Err(AgentCallError {
                        code: code.to_string(),
                        message: "no".to_string(),
                        retriable,
                    })
                }
            })
            .await;
            assert!(transport.singles().is_empty(), "{code}");
            assert_eq!(out.len(), 2);
            for (_, result) in &out {
                assert_eq!(result.as_ref().unwrap_err().code, code);
            }
        }
    }

    #[tokio::test]
    async fn a_unit_of_one_is_the_single_request() {
        let transport = Transport::new();
        let state = BatchState::new();
        let out = describe_unit(vec![pending("solo")], &state, |payload| {
            transport.sent.lock().unwrap().push(payload.clone());
            async move { single_answer(&payload) }
        })
        .await;
        assert_eq!(transport.batches(), 0);
        assert_eq!(transport.singles(), vec!["solo"]);
        assert_eq!(out[0].1.as_ref().unwrap(), "single for solo");
    }

    /// End to end through the mock lambda: a level of misses is described in
    /// batched requests and every function gets its intent and its hash.
    #[tokio::test]
    async fn generate_function_intents_batches_a_level_through_the_mock_lambda() {
        let _env = ENV_LOCK.lock().await;
        let prev_mock = std::env::var("CARRICK_MOCK_ALL").ok();
        let prev_size = std::env::var("CARRICK_INTENT_BATCH_SIZE").ok();
        // SAFETY: serialized on ENV_LOCK and restored below.
        unsafe {
            std::env::set_var("CARRICK_MOCK_ALL", "1");
            std::env::remove_var("CARRICK_INTENT_BATCH_SIZE");
        }
        let mut defs = HashMap::new();
        for i in 0..45 {
            let name = format!("fn{i}");
            let body = format!("const v = input{i};\nreturn transform(v);");
            defs.insert(name.clone(), def_with_body(&name, &body));
        }
        let before = crate::agent_service::request_counts();
        let agent = AgentService::new();
        generate_function_intents(
            &agent,
            &mut defs,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
        )
        .await;
        let after = crate::agent_service::request_counts();
        unsafe {
            match prev_mock {
                Some(v) => std::env::set_var("CARRICK_MOCK_ALL", v),
                None => std::env::remove_var("CARRICK_MOCK_ALL"),
            }
            if let Some(v) = prev_size {
                std::env::set_var("CARRICK_INTENT_BATCH_SIZE", v);
            }
        }
        for def in defs.values() {
            assert_eq!(
                def.intent.as_deref(),
                Some("Mock intent: function does something."),
                "{}",
                def.name
            );
            assert!(def.intent_input_hash.is_some());
        }
        let sent = |counts: &std::collections::BTreeMap<String, usize>| {
            counts.get("/generate-intent").copied().unwrap_or(0)
        };
        // Other tests in the process may call the mock too, so read a floor:
        // 45 functions at 20 per request is 3 requests, and never 45.
        let delta = sent(&after) - sent(&before);
        assert!((3..45).contains(&delta), "sent {delta} requests");
    }

    #[tokio::test]
    async fn level_results_stay_associated_when_calls_finish_out_of_order() {
        // Under `buffer_unordered` completion order is arbitrary. Force the
        // worst case: the first call finishes last, the last finishes first.
        let names = ["alpha", "beta", "gamma", "delta"];
        let level: Vec<Pending> = names.iter().map(|n| pending(n)).collect();
        let total = level.len() as u64;
        let completion_order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let observed = completion_order.clone();
        let outcomes = generate_level(level, 4, move |p| {
            let observed = observed.clone();
            async move {
                let position = names.iter().position(|n| *n == p.name).unwrap() as u64;
                tokio::time::sleep(std::time::Duration::from_millis((total - position) * 20)).await;
                observed.lock().unwrap().push(p.name.clone());
                let intent = format!("intent for {}", p.name);
                (p, Ok::<String, AgentCallError>(intent))
            }
        })
        .await;

        // The hazard is real: they did NOT complete in input order.
        assert_eq!(
            *completion_order.lock().unwrap(),
            vec!["delta", "gamma", "beta", "alpha"],
            "test did not actually exercise out-of-order completion"
        );

        // Output order is input order regardless, and every result is paired
        // with the function that produced it.
        let returned: Vec<&str> = outcomes.iter().map(|(p, _)| p.name.as_str()).collect();
        assert_eq!(returned, names);
        for (p, result) in &outcomes {
            assert_eq!(
                result.as_ref().unwrap(),
                &format!("intent for {}", p.name),
                "result was paired with the wrong function"
            );
            assert_eq!(p.hash, format!("hash-of-{}", p.name));
        }
    }

    #[tokio::test]
    async fn level_failures_are_isolated_to_their_own_function() {
        // One failing call must not cost its siblings their intents, and the
        // failure must arrive attached to the function that failed — that is
        // what leaves exactly that function with `intent = None` and no
        // content hash, so a rescan retries it alone.
        let level = vec![pending("ok_one"), pending("boom"), pending("ok_two")];

        let outcomes = generate_level(level, 2, |p| async move {
            if p.name == "boom" {
                let err = AgentCallError {
                    code: "model_error".to_string(),
                    message: "Gemini overloaded; retries exhausted".to_string(),
                    retriable: true,
                };
                return (p, Err(err));
            }
            let intent = format!("intent for {}", p.name);
            (p, Ok(intent))
        })
        .await;

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes[0].1.is_ok());
        assert_eq!(outcomes[1].0.name, "boom");
        let err = outcomes[1].1.as_ref().unwrap_err();
        // Transient class, so the summary counts it as failed-after-retry
        // rather than as a doomed-from-the-start quota abort.
        assert!(err.retriable);
        assert!(!err.is_quota_abort());
        assert!(outcomes[2].1.is_ok());
    }

    #[tokio::test]
    async fn intent_concurrency_knob_overrides_the_default() {
        let _env = ENV_LOCK.lock().await;
        let prev = std::env::var("CARRICK_INTENT_CONCURRENCY").ok();

        // SAFETY: env vars are process-global; ENV_LOCK serializes this
        // module's env-touching tests, and the var is restored before the
        // guard drops.
        unsafe {
            std::env::remove_var("CARRICK_INTENT_CONCURRENCY");
        }
        assert_eq!(intent_concurrency(), DEFAULT_INTENT_CONCURRENCY);

        unsafe {
            std::env::set_var("CARRICK_INTENT_CONCURRENCY", "3");
        }
        assert_eq!(intent_concurrency(), 3);

        // Zero would stall `buffer_unordered` forever; garbage falls back to
        // the default rather than failing the scan.
        unsafe {
            std::env::set_var("CARRICK_INTENT_CONCURRENCY", "0");
        }
        assert_eq!(intent_concurrency(), 1);
        unsafe {
            std::env::set_var("CARRICK_INTENT_CONCURRENCY", "lots");
        }
        assert_eq!(intent_concurrency(), DEFAULT_INTENT_CONCURRENCY);

        unsafe {
            match prev {
                Some(v) => std::env::set_var("CARRICK_INTENT_CONCURRENCY", v),
                None => std::env::remove_var("CARRICK_INTENT_CONCURRENCY"),
            }
        }
    }

    #[tokio::test]
    async fn bounded_concurrency_caps_calls_in_flight() {
        // The point of the fix: a level of 12 must never put more than the
        // configured number of requests on the backend at once.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let level: Vec<Pending> = (0..12).map(|i| pending(&format!("fn{}", i))).collect();
        let in_flight = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));

        let (in_flight_c, peak_c) = (in_flight.clone(), peak.clone());
        let outcomes = generate_level(level, 3, move |p| {
            let (in_flight, peak) = (in_flight_c.clone(), peak_c.clone());
            async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                (p, Ok::<String, AgentCallError>("intent".to_string()))
            }
        })
        .await;

        assert_eq!(outcomes.len(), 12);
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak in-flight was {}, expected at most 3",
            peak.load(Ordering::SeqCst)
        );
    }

    /// Started on its own task, intent generation hands back the definitions
    /// in the state the inline call left them: intents written, their hashes
    /// recorded, bodies stripped. The previous scan covers every function, so
    /// no lambda call is made.
    #[tokio::test]
    async fn intents_in_flight_return_the_definitions_the_inline_call_would() {
        let _env = ENV_LOCK.lock().await;
        let body = "const rate = table[region];\nreturn base * rate;";
        let mut defs = HashMap::new();
        defs.insert("helper".to_string(), def_with_body("helper", body));
        let hash = compute_intent_hash(body, &[]);
        let prev = HashMap::from([(hash.clone(), "applies a regional rate".to_string())]);

        let in_flight = IntentsInFlight::start(
            AgentService::new(),
            defs,
            previous_by_hash(prev),
            RunIntentMemo::default(),
        );
        let defs = in_flight.finish().await;

        assert_eq!(
            defs["helper"].intent.as_deref(),
            Some("applies a regional rate")
        );
        assert_eq!(
            defs["helper"].intent_input_hash.as_deref(),
            Some(hash.as_str())
        );
        assert!(defs["helper"].body_source.is_none());
    }

    // ------------------------------------- key material pinned (#1080)

    /// A recording `/generate-intent` transport for [`describe_functions`]:
    /// every request body it was sent, in send order, and an answer per
    /// function named `describes <name>`, batched or single. A name in `fail`
    /// is refused, alone or as part of any batch that carries it.
    #[derive(Clone, Default)]
    struct Recorder {
        sent: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        fail: std::sync::Arc<HashSet<String>>,
    }

    impl Recorder {
        fn failing(names: &[&str]) -> Self {
            Self {
                fail: std::sync::Arc::new(names.iter().map(|n| n.to_string()).collect()),
                ..Self::default()
            }
        }

        fn send(
            &self,
            payload: serde_json::Value,
        ) -> impl std::future::Future<Output = Result<String, AgentCallError>> + use<> {
            let recorder = self.clone();
            async move {
                recorder.sent.lock().unwrap().push(payload.clone());
                let names: Vec<String> = match payload.get("functions") {
                    Some(_) => batch_names(&payload),
                    None => vec![payload["name"].as_str().unwrap().to_string()],
                };
                if payload.get("functions").is_some() {
                    let rows: Vec<(String, Option<String>)> = names
                        .iter()
                        .map(|n| {
                            let intent =
                                (!recorder.fail.contains(n)).then(|| format!("describes {n}"));
                            (n.clone(), intent)
                        })
                        .collect();
                    let rows: Vec<(&str, Option<&str>)> = rows
                        .iter()
                        .map(|(n, i)| (n.as_str(), i.as_deref()))
                        .collect();
                    Ok(answer(&rows))
                } else if recorder.fail.contains(&names[0]) {
                    Err(AgentCallError {
                        code: "model_error".to_string(),
                        message: "retries exhausted".to_string(),
                        retriable: true,
                    })
                } else {
                    Ok(format!("describes {}", names[0]))
                }
            }
        }

        /// Every function name this recorder was asked to describe, in send
        /// order, whether it went alone or in a batch.
        fn asked(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .flat_map(|p| match p.get("functions") {
                    Some(_) => batch_names(p),
                    None => vec![p["name"].as_str().unwrap().to_string()],
                })
                .collect()
        }
    }

    /// Three levels over two files: leaves `a1`, `a2` (a.ts) and `b1` (b.ts),
    /// `mid` calling `a1` and `b1`, and `top` calling `mid` and `a2`.
    fn layered_fixture() -> HashMap<String, FunctionDefinition> {
        let in_file = |name: &str, file: &str, body: &str, calls: Vec<FunctionCallRef>| {
            let def = FunctionDefinition {
                file_path: file.into(),
                ..def_with_body(name, body)
            };
            (name.to_string(), with_calls(def, calls))
        };
        HashMap::from([
            in_file("a1", "a.ts", "const x = load(id);\nreturn x.total;", vec![]),
            in_file(
                "a2",
                "a.ts",
                "const y = parse(raw);\nreturn y.items;",
                vec![],
            ),
            in_file(
                "b1",
                "b.ts",
                "const r = await fetch(url);\nreturn r.json();",
                vec![],
            ),
            in_file(
                "mid",
                "b.ts",
                "const t = a1(id);\nconst u = b1(url);\nreturn { t, u };",
                vec![call_ref("a1", "a.ts", 1, 1), call_ref("b1", "b.ts", 1, 2)],
            ),
            in_file(
                "top",
                "c.ts",
                "const m = mid(id, url);\nreturn a2(m.raw);",
                vec![call_ref("mid", "b.ts", 4, 1), call_ref("a2", "a.ts", 2, 2)],
            ),
        ])
    }

    /// Run [`describe_functions`] with no previous scan, returning each
    /// request body as sent (sorted, since requests in a level race) and each
    /// function's `(name, intent, hash)`.
    async fn describe_layered_fixture(
        defs: &mut HashMap<String, FunctionDefinition>,
        recorder: &Recorder,
    ) -> (Vec<String>, Vec<(String, Option<String>, Option<String>)>) {
        describe_functions(
            defs,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
            |payload| recorder.send(payload),
        )
        .await;
        let mut bodies: Vec<String> = recorder
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.to_string())
            .collect();
        bodies.sort();
        let mut rows: Vec<(String, Option<String>, Option<String>)> = defs
            .iter()
            .map(|(k, d)| (k.clone(), d.intent.clone(), d.intent_input_hash.clone()))
            .collect();
        rows.sort();
        (bodies, rows)
    }

    /// The key material the scanner cache and the cloud `intentCacheKey` read
    /// is byte-identical to what main sent before carrick#1080: the same
    /// request bodies and the same content hashes, on a normal run through
    /// batching and three dependency levels. Every literal below was captured
    /// from main before the change.
    #[tokio::test]
    async fn a_normal_run_sends_the_request_bodies_and_hashes_main_sent() {
        let _env = ENV_LOCK.lock().await;
        assert_eq!(INTENT_CACHE_VERSION, 2);
        let mut defs = layered_fixture();
        let recorder = Recorder::default();
        let (bodies, rows) = describe_layered_fixture(&mut defs, &recorder).await;

        assert_eq!(
            bodies,
            vec![
                r#"{"body":"const m = mid(id, url);\nreturn a2(m.raw);","called_intents":["- mid: describes mid","- a2: describes a2"],"name":"top"}"#,
                r#"{"body":"const t = a1(id);\nconst u = b1(url);\nreturn { t, u };","called_intents":["- a1: describes a1","- b1: describes b1"],"name":"mid"}"#,
                r#"{"functions":[{"body":"const x = load(id);\nreturn x.total;","called_intents":[],"name":"a1"},{"body":"const y = parse(raw);\nreturn y.items;","called_intents":[],"name":"a2"},{"body":"const r = await fetch(url);\nreturn r.json();","called_intents":[],"name":"b1"}]}"#,
            ]
        );
        let row = |name: &str, hash: &str| {
            (
                name.to_string(),
                Some(format!("describes {name}")),
                Some(hash.to_string()),
            )
        };
        assert_eq!(
            rows,
            vec![
                row(
                    "a1",
                    "caf526315c7adeef18ea44a90ca74e8a7149a0cd9783ffd290537b81bb26c59a"
                ),
                row(
                    "a2",
                    "ab79db251f5dea5aba9b0fcc7e4293f6e22bfc560cf5b9d6b2044bedb9732777"
                ),
                row(
                    "b1",
                    "1658b50668d7b279da90f2b55b43267bbd5e1e02ca1bd72afde41d3a76e842cf"
                ),
                row(
                    "mid",
                    "63345450bd79e3a36e8c2e7312615a1017912bb7ae3a908a082db0b9b4e48810"
                ),
                row(
                    "top",
                    "aeed9070a2b8c8d7cba07ac583751857def81412865b6e7d8bb6333917559f1b"
                ),
            ]
        );
    }

    /// The blob a scan would upload for `defs`: file paths as they were, bodies
    /// already stripped, which is all [`PreviousIntents`] reads.
    fn previous_scan(defs: &HashMap<String, FunctionDefinition>) -> PreviousIntents {
        PreviousIntents::from_definitions(defs)
    }

    /// carrick#1080 D7. `a1` fails. `mid` calls it and `top` calls `mid`, so
    /// both are deferred: no request carries either of them, `mid` keeps the
    /// intent and hash the previous scan gave it, and `top`, which had none,
    /// stays without one. `a2` and `b1` are unaffected.
    #[tokio::test]
    async fn a_failed_callee_defers_every_transitive_caller() {
        let _env = ENV_LOCK.lock().await;
        let mut prev_defs = layered_fixture();
        let mid = prev_defs.get_mut("mid").unwrap();
        mid.intent = Some("combines a total and a fetched body".to_string());
        mid.intent_input_hash = Some("hash-of-mid-last-scan".to_string());
        let previous = previous_scan(&prev_defs);

        let mut defs = layered_fixture();
        let recorder = Recorder::failing(&["a1"]);
        describe_functions(&mut defs, &previous, &RunIntentMemo::default(), |p| {
            recorder.send(p)
        })
        .await;

        // The batch left `a1` unanswered, so it was sent again alone and
        // refused; neither caller was ever sent.
        assert_eq!(recorder.asked(), vec!["a1", "a2", "b1", "a1"]);

        assert!(defs["a1"].intent.is_none());
        assert!(defs["a1"].intent_input_hash.is_none());
        assert_eq!(
            defs["mid"].intent.as_deref(),
            Some("combines a total and a fetched body")
        );
        assert_eq!(
            defs["mid"].intent_input_hash.as_deref(),
            Some("hash-of-mid-last-scan")
        );
        assert!(defs["top"].intent.is_none());
        assert!(defs["top"].intent_input_hash.is_none());
        assert_eq!(defs["a2"].intent.as_deref(), Some("describes a2"));
        assert_eq!(defs["b1"].intent.as_deref(), Some("describes b1"));
        assert!(defs.values().all(|d| d.body_source.is_none()));
    }

    /// carrick#1080 D7, the scan after. The callee answers now: it is asked
    /// first, then each deferred caller once, in dependency order, and every
    /// caller is keyed on the callee's intent. The functions described last
    /// scan replay from cache. The hashes are the ones a run with no failure
    /// writes, so nothing was keyed on the missing callee along the way.
    #[tokio::test]
    async fn the_scan_after_asks_the_callee_then_its_callers_once() {
        let _env = ENV_LOCK.lock().await;
        let mut first = layered_fixture();
        let failing = Recorder::failing(&["a1"]);
        describe_functions(
            &mut first,
            &PreviousIntents::default(),
            &RunIntentMemo::default(),
            |p| failing.send(p),
        )
        .await;
        // The first index had no previous scan, so nothing was kept.
        assert!(first["mid"].intent.is_none());
        assert!(first["top"].intent.is_none());

        let mut second = layered_fixture();
        let recorder = Recorder::default();
        describe_functions(
            &mut second,
            &previous_scan(&first),
            &RunIntentMemo::default(),
            |p| recorder.send(p),
        )
        .await;

        let asked = recorder.asked();
        let position = |name: &str| {
            let hits: Vec<usize> = asked
                .iter()
                .enumerate()
                .filter(|(_, n)| n.as_str() == name)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                hits.len(),
                1,
                "{name} asked {} times in {asked:?}",
                hits.len()
            );
            hits[0]
        };
        assert!(position("a1") < position("mid"));
        assert!(position("mid") < position("top"));
        assert_eq!(asked.len(), 3, "a2 and b1 replay from cache: {asked:?}");
        assert!(failing.asked().iter().all(|n| n != "mid" && n != "top"));

        // Exactly the rows a run with no failure writes.
        let mut clean = layered_fixture();
        let (_, clean_rows) = describe_layered_fixture(&mut clean, &Recorder::default()).await;
        let mut rows: Vec<(String, Option<String>, Option<String>)> = second
            .iter()
            .map(|(k, d)| (k.clone(), d.intent.clone(), d.intent_input_hash.clone()))
            .collect();
        rows.sort();
        assert_eq!(rows, clean_rows);
    }

    /// carrick#1080 D6. Two workspace members hold the same functions. The
    /// first member describes them; the second sends nothing, and its rows
    /// carry the same intents and hashes, callers included.
    #[tokio::test]
    async fn a_function_two_members_share_is_described_once() {
        let _env = ENV_LOCK.lock().await;
        let memo = RunIntentMemo::default();
        let recorder = Recorder::default();

        let mut member_a = layered_fixture();
        describe_functions(&mut member_a, &PreviousIntents::default(), &memo, |p| {
            recorder.send(p)
        })
        .await;
        let after_a = recorder.asked().len();
        assert_eq!(after_a, 5);

        // The second member's copies live under its own directory; the
        // request carries only name, body and callee intents, so they are the
        // same requests.
        let mut member_b: HashMap<String, FunctionDefinition> = layered_fixture()
            .into_iter()
            .map(|(key, mut def)| {
                def.file_path = std::path::Path::new("packages/b").join(&def.file_path);
                for call in &mut def.calls {
                    call.file_path = format!("packages/b/{}", call.file_path);
                }
                (key, def)
            })
            .collect();
        describe_functions(&mut member_b, &PreviousIntents::default(), &memo, |p| {
            recorder.send(p)
        })
        .await;

        assert_eq!(recorder.asked().len(), after_a, "member b sent a request");
        for (key, def) in &member_b {
            assert_eq!(def.intent, member_a[key].intent, "{key}");
            assert!(def.intent.is_some(), "{key}");
            assert_eq!(
                def.intent_input_hash, member_a[key].intent_input_hash,
                "{key}"
            );
        }
    }

    /// A service whose analysis fails returns before it collects its intents;
    /// the dropped handle must stop the task rather than let it keep calling
    /// the model for a result nobody reads.
    #[tokio::test]
    async fn dropping_an_unjoined_task_aborts_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let task = AbortOnDrop(Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
        })));

        // Let the task start and park in its sleep, so the drop has to stop a
        // task that is running, not one that has never been polled.
        tokio::task::yield_now().await;
        drop(task);
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        assert!(
            !finished.load(Ordering::SeqCst),
            "the task ran to completion after its handle was dropped"
        );
    }

    /// A panic inside the task surfaces where the value is collected, as it
    /// would have had the work run inline.
    #[tokio::test]
    #[should_panic(expected = "intent stage blew up")]
    async fn a_panic_in_the_task_resumes_at_join() {
        let task: AbortOnDrop<()> = AbortOnDrop(Some(tokio::spawn(async {
            panic!("intent stage blew up");
        })));
        task.join().await;
    }
}
