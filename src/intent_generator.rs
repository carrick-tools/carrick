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
use crate::visitor::{FunctionCallRef, FunctionDefinition, ImportedSymbol};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
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

/// Build a `content_hash -> intent` map from a previous scan's function
/// definitions, keeping only entries that carry both an intent and the hash of
/// the inputs that produced it. Passed into [`generate_function_intents`] so an
/// unchanged function (same body + same callee intents) reuses its prior intent
/// without another `/generate-intent` call. Definitions from a scan that
/// predates content hashing simply lack `intent_input_hash` and are skipped
/// (treated as cache misses).
pub fn intents_by_hash(
    function_definitions: &HashMap<String, FunctionDefinition>,
) -> HashMap<String, String> {
    function_definitions
        .values()
        .filter_map(|def| match (&def.intent_input_hash, &def.intent) {
            (Some(hash), Some(intent)) => Some((hash.clone(), intent.clone())),
            _ => None,
        })
        .collect()
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

/// True when `body` references `name` as a standalone JS identifier.
///
/// A plain `contains` over-matches short names — `id` inside `userId`, `get`
/// inside `getUser`, names inside comments notwithstanding — which fabricates
/// call-graph edges: callers fold phantom callees' intents into their content
/// hash (needless regeneration) and phantom back-edges create cycles that dump
/// real functions into the unordered final topological level (#55, #141). A
/// match counts only when not flanked by identifier characters (`[A-Za-z0-9_$]`).
fn body_references_identifier(body: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let is_ident_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut search_from = 0;
    while let Some(pos) = body[search_from..].find(name) {
        let start = search_from + pos;
        let end = start + name.len();
        let before_ok = !body[..start].chars().next_back().is_some_and(is_ident_char);
        let after_ok = !body[end..].chars().next().is_some_and(is_ident_char);
        if before_ok && after_ok {
            return true;
        }
        // Overlap-safe: re-search from the next character of this match.
        search_from = start + name.chars().next().map_or(1, |c| c.len_utf8());
    }
    false
}

/// Does `body` (the body of the function keyed `caller_key`) reference the
/// local function keyed `candidate_key`?
///
/// Class members are keyed as `Class.member` (see FunctionDefinitionExtractor;
/// a static colliding with a same-named instance member is `Class.static.member`),
/// but call sites reference the bare member name (`foo(...)`, `this.foo(...)`,
/// `Class.foo(...)`) — never the dotted key as one identifier. So identifier
/// matching runs on the bare name (the LAST segment), and a method callee
/// additionally requires class evidence from the FIRST segment: either the same
/// enclosing class as the caller (the `this.foo()` case) or a reference to its
/// class name in the body (the `Class.foo()` case).
/// Module-level functions keep the exact pre-existing behaviour.
fn is_local_callee(caller_key: &str, body: &str, candidate_key: &str) -> bool {
    fn bare_name(key: &str) -> &str {
        key.rsplit_once('.').map_or(key, |(_, tail)| tail)
    }
    fn class_of(key: &str) -> Option<&str> {
        key.split_once('.').map(|(head, _)| head)
    }
    candidate_key != caller_key
        && body_references_identifier(body, bare_name(candidate_key))
        && match class_of(candidate_key) {
            None => true,
            Some(callee_class) => {
                class_of(caller_key) == Some(callee_class)
                    || body_references_identifier(body, callee_class)
            }
        }
}

/// A cache-miss function awaiting its `/generate-intent` call: everything the
/// payload needs, plus the content hash to persist if the call succeeds.
struct Pending {
    name: String,
    body: String,
    called_intents: Vec<String>,
    hash: String,
}

/// Concurrent `/generate-intent` calls in flight per dependency level when
/// `CARRICK_INTENT_CONCURRENCY` is unset.
///
/// Deliberately below the shared `CARRICK_CONCURRENCY_LIMIT` (20) that file
/// analysis runs at. Intent calls outnumber file-analysis calls by an order of
/// magnitude on a function-dense repo — roughly one per function rather than
/// one per file — so the same in-flight count is a far higher sustained
/// request rate against the same backend quota, which is what produced the
/// 429-wrapped 503s in #460.
const DEFAULT_INTENT_CONCURRENCY: usize = 8;

/// In-flight `/generate-intent` calls allowed per level.
///
/// `CARRICK_INTENT_CONCURRENCY` overrides the default. Raising it above
/// `CARRICK_CONCURRENCY_LIMIT` has no effect: `AgentService` holds a semaphore
/// at that count, so it is the hard ceiling for every lambda call.
fn intent_concurrency() -> usize {
    std::env::var("CARRICK_INTENT_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_INTENT_CONCURRENCY)
        .max(1)
}

/// Dispatch one dependency level's calls at most `concurrency` at a time,
/// returning each `Pending` paired with its own result **in input order**.
///
/// Completion order under `buffer_unordered` is arbitrary — a slow call
/// finishes after ones queued behind it. Results are therefore carried
/// alongside the `Pending` that produced them and re-sorted by input position,
/// so neither the fold nor any future caller can associate an intent with the
/// wrong function, and a re-run over the same level produces the same sequence
/// regardless of backend timing.
async fn generate_level<F, Fut>(
    pending: Vec<Pending>,
    concurrency: usize,
    call: F,
) -> Vec<(Pending, Result<String, AgentCallError>)>
where
    F: Fn(Pending) -> Fut,
    Fut: std::future::Future<Output = (Pending, Result<String, AgentCallError>)>,
{
    let mut results: Vec<(usize, Pending, Result<String, AgentCallError>)> =
        futures::stream::iter(pending.into_iter().enumerate().map(|(idx, item)| {
            let fut = call(item);
            async move {
                let (item, result) = fut.await;
                (idx, item, result)
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;

    results.sort_by_key(|(idx, _, _)| *idx);
    results
        .into_iter()
        .map(|(_, item, result)| (item, result))
        .collect()
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
/// `prev_intents_by_hash` is a `content_hash -> intent` map from the previous
/// scan (see [`intents_by_hash`]). A function whose freshly-computed hash is
/// present in the map reuses that intent without calling `/generate-intent`.
/// Pass an empty map for a full (non-incremental) scan.
pub async fn generate_function_intents(
    agent_service: &AgentService,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    _imported_symbols: &HashMap<String, ImportedSymbol>,
    prev_intents_by_hash: &HashMap<String, String>,
) {
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

    // Build a local call graph: for each function, which other local functions does it reference?
    let local_fn_names: HashSet<&str> = function_definitions.keys().map(|s| s.as_str()).collect();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();

    for name in &eligible {
        if let Some(def) = function_definitions.get(name)
            && let Some(ref body) = def.body_source
        {
            let called: Vec<String> = local_fn_names
                .iter()
                .filter(|&&fn_name| is_local_callee(name, body, fn_name))
                .map(|&s| s.to_string())
                .collect();
            deps.insert(name.clone(), called);
        }
    }

    // Populate the `calls` field on each function with references to callees
    for name in &eligible {
        if let Some(called) = deps.get(name) {
            let call_refs: Vec<FunctionCallRef> = called
                .iter()
                .filter_map(|callee_name| {
                    function_definitions
                        .get(callee_name)
                        .map(|callee_def| FunctionCallRef {
                            name: callee_name.clone(),
                            file_path: callee_def.file_path.to_string_lossy().to_string(),
                            line_number: callee_def.line_number,
                        })
                })
                .collect();
            if let Some(def) = function_definitions.get_mut(name) {
                def.calls = call_refs;
            }
        }
    }

    // CARRICK_SKIP_INTENTS: stop before any /generate-intent lambda call.
    // Intents are one LLM call per eligible function — the dominant cost of
    // scanning a large repo — and feed only the MCP index; no cross-repo
    // analysis or eval dimension consumes them. Everything deterministic has
    // already happened above (`calls` is populated), and body_source is still
    // stripped (source stays in GitHub, not AWS).
    if std::env::var("CARRICK_SKIP_INTENTS").is_ok() {
        debug!(
            "CARRICK_SKIP_INTENTS set — skipping intent generation for {} function(s)",
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
    let mut intents: HashMap<String, String> = HashMap::new();
    let mut hashes: HashMap<String, String> = HashMap::new();
    let mut reused = 0usize;
    let mut generated = 0usize;

    for (level_idx, level) in levels.iter().enumerate() {
        // Compute each function's called_intents context and content hash, then
        // split into cache hits (reuse) and misses (call the lambda).
        let mut to_generate: Vec<Pending> = Vec::new();

        for name in level {
            let Some(def) = function_definitions.get(name) else {
                continue;
            };
            let Some(body) = def.body_source.as_ref() else {
                continue;
            };

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

            if let Some(prev_intent) = prev_intents_by_hash.get(&hash) {
                // Identical body + callee context as a prior scan — reuse.
                intents.insert(name.clone(), prev_intent.clone());
                hashes.insert(name.clone(), hash);
                reused += 1;
            } else {
                to_generate.push(Pending {
                    name: name.clone(),
                    body: body.clone(),
                    called_intents,
                    hash,
                });
            }
        }

        if to_generate.is_empty() {
            continue;
        }

        // Run this level's cache-miss lambda calls with bounded concurrency
        // (#460). Every call in a level is independent, so the old unbounded
        // `join_all` queued the whole level at once; on a function-dense repo
        // that is thousands of simultaneous requests, and the backend answers
        // the overflow with a 429-wrapped 503 that costs those functions their
        // intents.
        let attempted = to_generate.len();
        let outcomes = generate_level(to_generate, intent_concurrency(), |pending| async move {
            let payload = serde_json::json!({
                "name": pending.name,
                "body": pending.body,
                "called_intents": pending.called_intents,
            });
            let result = agent_service
                .post_to_lambda("/generate-intent", &payload, &pending.name)
                .await;
            (pending, result)
        })
        .await;

        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let mut aborted = 0usize;

        for (pending, result) in outcomes {
            let Pending { name, hash, .. } = pending;
            match result {
                Ok(intent) => {
                    let intent = intent.trim().to_string();
                    if !intent.is_empty() && intent.len() < 500 {
                        hashes.insert(name.clone(), hash);
                        intents.insert(name, intent);
                        generated += 1;
                        succeeded += 1;
                    } else {
                        // Empty or over-long response: drop it. The function
                        // keeps `intent = None`, so it (and its callers) retry
                        // next scan. Log it — otherwise this is a silent,
                        // permanent cache miss.
                        warn!(
                            "Discarding intent for {} ({} chars, expected 1..500)",
                            name,
                            intent.len()
                        );
                        failed += 1;
                    }
                }
                Err(e) => {
                    // Degrade gracefully: no intent and no content hash is
                    // written, so the next scan retries exactly this function
                    // and replays the rest from cache.
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

    // Write resolved intents and their content hashes back to the definitions.
    let total = intents.len();
    for (name, intent) in intents {
        if let Some(def) = function_definitions.get_mut(&name) {
            def.intent = Some(intent);
            def.intent_input_hash = hashes.get(&name).cloned();
        }
    }

    debug!(
        "Intents: {} total ({} reused from content-hash cache, {} freshly generated)",
        total, reused, generated
    );

    // Strip body_source — source code stays in GitHub, not AWS
    strip_body_source(function_definitions);
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

    #[test]
    fn identifier_match_requires_word_boundaries() {
        // Substrings of longer identifiers are not references (#55, #141).
        assert!(!body_references_identifier("return userId;", "id"));
        assert!(!body_references_identifier("return getUser();", "get"));
        assert!(!body_references_identifier("run_all();", "run"));
        assert!(!body_references_identifier("fetchData$();", "fetchData"));

        // Real references still match.
        assert!(body_references_identifier("return id;", "id"));
        assert!(body_references_identifier("const x = get();", "get"));
        assert!(body_references_identifier("await helper(1)", "helper"));
        // Passed as a callback — still a dependency for intent purposes.
        assert!(body_references_identifier("arr.map(helper)", "helper"));
        // Boundary positions: start and end of the body.
        assert!(body_references_identifier("helper()", "helper"));
        assert!(body_references_identifier("return helper", "helper"));
    }

    #[test]
    fn identifier_match_finds_later_occurrence_after_substring_hit() {
        // First occurrence is embedded in a longer identifier; a later
        // standalone occurrence must still be found.
        assert!(body_references_identifier("getUser(); get();", "get"));
        // Overlap-safety: "aa" inside "aaa" — no standalone "aa" with
        // boundaries on both sides.
        assert!(!body_references_identifier("aaab", "aa"));
    }

    #[test]
    fn phantom_substring_edges_no_longer_create_cycles() {
        // `processId` contains "id"; with substring matching, `id` ← processId
        // plus a real processId ← id edge formed a fake cycle that dumped both
        // functions into the unordered cycle level.
        let names = vec!["id".to_string(), "processId".to_string()];
        let mut defs = HashMap::new();
        defs.insert("id".to_string(), def_with_body("id", "return 1;"));
        defs.insert(
            "processId".to_string(),
            def_with_body("processId", "return id();"),
        );

        let mut deps = HashMap::new();
        for name in &names {
            let body = defs[name].body_source.as_ref().unwrap();
            let called: Vec<String> = names
                .iter()
                .filter(|n| n.as_str() != name && body_references_identifier(body, n))
                .cloned()
                .collect();
            deps.insert(name.clone(), called);
        }

        assert_eq!(deps["id"], Vec::<String>::new());
        assert_eq!(deps["processId"], vec!["id".to_string()]);

        let levels = topological_levels(&names, &deps);
        assert_eq!(levels.len(), 2, "leaf level then caller level, no cycle");
        assert_eq!(levels[0], vec!["id".to_string()]);
        assert_eq!(levels[1], vec!["processId".to_string()]);
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
                return_type: None,
                return_is_explicit: false,
                signature: None,
                intent_input_hash: None,
            },
        );
        strip_body_source(&mut defs);
        assert!(defs.get("foo").unwrap().body_source.is_none());
        // Intent should be preserved
        assert!(defs.get("foo").unwrap().intent.is_some());
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
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
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

        let map = intents_by_hash(&defs);
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("abc123").map(String::as_str),
            Some("does the thing")
        );
    }

    /// Env vars are process-global and tests run in parallel: every test in
    /// THIS module that sets a CARRICK_* flag — or calls
    /// generate_function_intents while another of them could have one set —
    /// serializes on this lock (it is module-private, not a crate-wide
    /// guarantee). Tokio's mutex, so the guard may be held across await
    /// points.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
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
        defs.insert("main".to_string(), def_with_body("main", main_body));

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
            &HashMap::<String, ImportedSymbol>::new(),
            &prev,
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
            &HashMap::<String, ImportedSymbol>::new(),
            &HashMap::new(),
        )
        .await;

        assert!(defs["getId"].intent.is_none());
        assert!(defs["getId"].intent_input_hash.is_none());
        assert!(defs["getId"].body_source.is_none());
    }

    /// CARRICK_SKIP_INTENTS stops intent generation before any lambda call
    /// while keeping the deterministic parts: `calls` is populated and
    /// body_source is stripped. Both cases run inside one test (sequentially)
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
            defs.insert("main".to_string(), def_with_body("main", main_body));
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
            &HashMap::<String, ImportedSymbol>::new(),
            &HashMap::new(),
        )
        .await;
        unsafe {
            std::env::remove_var("CARRICK_SKIP_INTENTS");
        }

        // No intents, no hashes — the lambda path never ran.
        assert!(defs["helper"].intent.is_none());
        assert!(defs["main"].intent.is_none());
        assert!(defs["helper"].intent_input_hash.is_none());
        // Deterministic outputs are intact: caller→callee edge + stripping.
        assert_eq!(defs["main"].calls.len(), 1);
        assert_eq!(defs["main"].calls[0].name, "helper");
        assert!(defs["helper"].body_source.is_none());
        assert!(defs["main"].body_source.is_none());

        // Control: with the flag unset (MOCK_ALL still on), intents flow.
        let mut defs = make_defs();
        generate_function_intents(
            &agent,
            &mut defs,
            &HashMap::<String, ImportedSymbol>::new(),
            &HashMap::new(),
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

    #[test]
    fn method_callee_matches_this_calls_within_the_same_class() {
        // `Presenter.toJson` calls `this.isFinished(...)`: same class, so the
        // bare-name match is enough.
        assert!(is_local_callee(
            "Presenter.toJson",
            "return { done: this.isFinished(status) };",
            "Presenter.isFinished"
        ));
        // Private members work the same way (`this.#reload()`).
        assert!(is_local_callee(
            "Job.run",
            "await this.#reload();",
            "Job.#reload"
        ));
    }

    #[test]
    fn method_callee_matches_static_calls_via_class_name() {
        assert!(is_local_callee(
            "serialiseRun",
            "return Presenter.isFinished(status);",
            "Presenter.isFinished"
        ));
    }

    #[test]
    fn method_callee_requires_class_evidence_across_classes() {
        // Another class has a same-named method; a bare `this.create(...)`
        // in an unrelated class must not link to it.
        assert!(!is_local_callee(
            "OrderController.submit",
            "return this.create(payload);",
            "UserService.create"
        ));
        // ...but naming the class is evidence enough.
        assert!(is_local_callee(
            "OrderController.submit",
            "return UserService.create(payload);",
            "UserService.create"
        ));
    }

    #[test]
    fn collision_qualified_static_keys_resolve_class_from_first_segment() {
        // `Counter.static.create` (a static colliding with an instance member):
        // bare name is the LAST segment, class evidence the FIRST.
        assert!(is_local_callee(
            "main",
            "return Counter.create(1);",
            "Counter.static.create"
        ));
        // Same-class caller links too.
        assert!(is_local_callee(
            "Counter.report",
            "return Counter.create(1);",
            "Counter.static.create"
        ));
        // No class evidence, no edge.
        assert!(!is_local_callee(
            "Other.run",
            "return this.create(1);",
            "Counter.static.create"
        ));
    }

    #[test]
    fn module_level_callees_keep_pre_existing_behaviour() {
        assert!(is_local_callee("main", "return helper();", "helper"));
        assert!(!is_local_callee("main", "return userHelper();", "helper"));
        // Self-reference is never an edge.
        assert!(!is_local_callee("helper", "return helper();", "helper"));
        // A method body referencing a module-level function links normally.
        assert!(is_local_callee("Svc.run", "return helper();", "helper"));
    }

    fn pending(name: &str) -> Pending {
        Pending {
            name: name.to_string(),
            body: format!("return {}();", name),
            called_intents: vec![],
            hash: format!("hash-of-{}", name),
        }
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
                (p, Ok(intent))
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
                (p, Ok("intent".to_string()))
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
}
