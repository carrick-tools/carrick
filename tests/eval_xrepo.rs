//! Live cross-repo accuracy scorer (cross-repo eval S4, #223 — full vector).
//!
//! Scores the *real* scanner over the authored `xrepo-corpus-1` constellation and
//! reports the **full per-metric correctness vector**, **report-only** (a monitor,
//! never a gate). Every metric is the S0 scorer contract §6 row of the same name:
//!
//!   1. **endpoint-set P/R/F1** — `expected.json` producers vs the joined
//!      [`EvalProjection`]'s `endpoints`. HTTP compared as normalized
//!      `(METHOD, norm_path(path))`; GraphQL/socket producers compared on the
//!      canonical `OperationKey` string (which carries kind/direction). Scored
//!      corpus-wide (the joined projection carries no per-repo provenance on ops).
//!   2. **call-set P/R/F1** — `expected.json` consumers vs `EvalProjection.calls`.
//!      HTTP uses the Tier-A fuzzy match (`method ==` AND `host_contains` ⊂ key
//!      AND `path_contains` ⊂ path); GraphQL/socket consumers compare on the
//!      canonical key.
//!   3. **negative `_must_not_emit`** (row 3) — count actual ops whose normalized
//!      `(method, path)` ∈ a repo's `_must_not_emit` list → `decoy_leak`.
//!   4. **owner accuracy** (row 4) — `expected.owner == actual.handler` over
//!      endpoints present in BOTH sets (keyed).
//!   5. **type-anchor accuracy** (row 5) — `expected.primary_type_symbol ==
//!      actual.primary_type_symbol` (null==null ok) over ops in both sets.
//!   6. **type-resolution correctness** (row 6) — `type_state` exact eq AND
//!      resolved-type eq up to whitespace and union member order, over ops in
//!      both sets that carry an expected resolved type.
//!   7. **cross-repo match P/R/F1** (row 7) — `expected-output.json.matches` vs
//!      `EvalProjection.cross_repo_matches`, keyed by
//!      `(producer_repo, norm(producer_key), consumer_repo, norm(consumer_key))`,
//!      spanning HTTP + GraphQL + socket edges. **Repo identity is the corpus
//!      repo DIR name** (#431): the projection's `producer_repo`/`consumer_repo`
//!      carry the #368 `service_name ?? repo_name` id, so any service name
//!      declared in a corpus repo's `carrick.json` is folded to its owning repo
//!      dir on BOTH sides of the join before scoring (see [`load_repo_aliases`]).
//!      Labels may therefore key an edge by either the repo dir or the service
//!      name; both normalize to the same identity.
//!   8. **compat-verdict accuracy** (row 8) — `expected.type_compatible ==
//!      actual.type_compatible` over edges in both sets. Guarded by §7: if any
//!      labelled edge expects a non-null verdict, the actual matches MUST contain
//!      ≥1 with `type_compatible.is_some()`, else the scorer **fails loud**.
//!   9. **dependency conflicts** (row 9) — `(package, sorted versions, severity)`
//!      set equality, expected vs actual.
//!  10. **orphans** (row 10) — `(repo, side, norm key)` set equality over expected
//!      orphans vs actual unmatched producers/consumers.
//!
//! Every metric is reported as **mean ± sample sd over N**, **partitioned
//! `capability` vs `roadmap`** (every corpus label is `capability` today, so the
//! `roadmap` partition is empty — the partitioning is wired, not a no-op for the
//! day a `roadmap` label lands). N runs (default 5, `CARRICK_EVAL_RUNS`). One
//! [`EvalRunRecord`] JSONL row (`tier="xrepo"`, `corpus="xrepo-corpus-1"`) is
//! written + echoed for the Axiom history, with the full §6-row vector populated
//! into the S6 record fields.
//!
//! **DEFERRED:** the full monitor cadence (debounced-on-main + issue-on-regression)
//! from #203 is NOT in this slice — it stays a follow-up (build plan §7 slice 4 /
//! #203). This slice ships the metric vector + `workflow_dispatch` only.
//!
//! ## LIVE vs offline
//! The scored run uses the **real LLM** (no `CARRICK_MOCK_ALL`), so it only runs
//! in CI with GitHub Actions OIDC granting the scanner its keyless cloud auth. It
//! reuses the S2 two-phase `LocalDirStorage` harness (Phase A isolated per repo,
//! Phase B joins) — but in LIVE mode, with a *distinct* env policy: see
//! [`strip_ci_identity_keep_oidc`]. Gated behind `#[ignore]` + `CARRICK_EVAL_LIVE`
//! so plain `cargo test` never triggers a costly scan; the scoring-math is covered
//! by the cheap `#[cfg(test)]`-style unit tests that DO run normally.
//!
//! Runner: `.github/workflows/eval-xrepo.yml` (`workflow_dispatch`).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The corpus fixture directory name. Defaults to `xrepo-corpus-1` so the
/// original corpus runs unchanged; set `CARRICK_EVAL_CORPUS` to point the
/// harness at a different authored corpus under `tests/fixtures/`. The value is
/// run-wide config, so it is read from the environment exactly once per process
/// (a later mutation — e.g. a parallel test setting the var — cannot change it).
fn corpus_name() -> String {
    static CORPUS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CORPUS
        .get_or_init(|| {
            std::env::var("CARRICK_EVAL_CORPUS").unwrap_or_else(|_| "xrepo-corpus-1".to_string())
        })
        .clone()
}
const DEFAULT_RUNS: usize = 5;

// ---------------------------------------------------------------------------
// Shared primitives (contract §1) — mirrors of the Tier-A scorer's helpers.
// Kept local because integration tests are separate crates and cannot share a
// private test module; these are intentionally identical to `eval_tier_a.rs`.
// ---------------------------------------------------------------------------

/// Collapse `:x` / `{x}` / `[x]` param syntaxes to `:param` and strip a single
/// trailing slash (contract §1.1). This is what makes the `:id`/`{id}`/`[id]`
/// corpus traps (#167) score as matches rather than spurious mismatches.
fn norm_path(p: &str) -> String {
    let mut out = String::new();
    for (i, seg) in p.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        let is_param = seg.starts_with(':')
            || (seg.starts_with('{') && seg.ends_with('}'))
            || (seg.starts_with('[') && seg.ends_with(']'));
        out.push_str(if is_param { ":param" } else { seg });
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Normalize a canonical operation key `"<protocol>|<METHOD>|<path>"` (contract
/// §1.2) by running its path segment through [`norm_path`]. A key without the two
/// pipes is returned unchanged (defensive; the corpus always supplies the full
/// form). For `graphql|<kind>|<field>` and `socket|<dir>|<event>` the third
/// segment has no path params, so `norm_path` is a no-op on it — the function is
/// uniform across protocols.
fn norm_key(key: &str) -> String {
    let mut parts = key.splitn(3, '|');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(proto), Some(method), Some(path)) => {
            format!("{proto}|{}|{}", method.to_uppercase(), norm_path(path))
        }
        _ => key.to_string(),
    }
}

/// Collapse all runs of ASCII whitespace to a single space and trim, for the
/// resolved-type comparison (contract §6 row 6: "whitespace-collapsed resolved
/// -type eq"). Authors and the sidecar disagree only on incidental spacing.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A rendered type text reduced to a form that does not depend on the order a
/// union's members were written in (carrick#735).
///
/// A union is a set. The sidecar now prints its members in a canonical order,
/// which is not the order the corpus labels were authored in — those record
/// what the compiler printed on the day they were written. The answer key is
/// not edited to chase a printer change, so the COMPARISON absorbs the
/// difference instead: both sides are reduced here, and two texts are equal iff
/// they carry the same members at every nesting level.
///
/// The order imposed here is plain lexicographic and deliberately does NOT
/// mirror the sidecar's (intrinsics by compiler id, then by text). It does not
/// have to: both sides go through this function, so any total order decides
/// equality the same way, and a label written in an order the printer would
/// never emit still scores as equal to the type it names.
///
/// Only union order is normalised. Property order, names, optionality and
/// member texts are compared as written — reordering those would weaken the
/// metric rather than describe the same type.
fn canonical_type_text(s: &str) -> String {
    canon_type(&collapse_ws(s))
}

/// Canonicalise one type expression: split its top-level union members, reduce
/// each, sort them.
fn canon_type(s: &str) -> String {
    let s = s.trim();
    let parts = split_top_level(s, '|');
    if parts.len() > 1 {
        let mut members: Vec<String> = parts.iter().map(|p| canon_type(p)).collect();
        members.sort();
        return members.join(" | ");
    }
    canon_atom(s)
}

/// Canonicalise a single (non-union) type expression by descending into the
/// structures that can hold a union: object bodies, parenthesised groups,
/// tuples, and generic arguments. Anything else — a name, a literal, a function
/// signature — is compared as written.
fn canon_atom(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    // A function type's `=>` makes its parentheses a parameter list, not a
    // group. Left verbatim rather than mis-parsed.
    if find_top_level(s, "=>").is_some() {
        return s.to_string();
    }

    let mut core = s;
    let mut suffix = String::new();
    while core.ends_with("[]") {
        core = core[..core.len() - 2].trim_end();
        suffix.push_str("[]");
    }

    if is_wrapped(core, '(', ')') {
        return format!("({}){}", canon_type(&core[1..core.len() - 1]), suffix);
    }
    if is_wrapped(core, '{', '}') {
        let inner = &core[1..core.len() - 1];
        let members: Vec<String> = split_top_level(inner, ';')
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .map(|m| canon_member(&m))
            .collect();
        if members.is_empty() {
            return format!("{{}}{suffix}");
        }
        return format!("{{ {}; }}{}", members.join("; "), suffix);
    }
    if is_wrapped(core, '[', ']') {
        let elements: Vec<String> = split_top_level(&core[1..core.len() - 1], ',')
            .iter()
            .map(|e| canon_type(e))
            .collect();
        return format!("[{}]{}", elements.join(", "), suffix);
    }
    if let Some(open) = generic_open(core) {
        let name = core[..open].trim();
        let args: Vec<String> = split_top_level(&core[open + 1..core.len() - 1], ',')
            .iter()
            .map(|a| canon_type(a))
            .collect();
        return format!("{}<{}>{}", name, args.join(", "), suffix);
    }
    format!("{core}{suffix}")
}

/// Canonicalise one object member: the key as written, the type reduced.
fn canon_member(member: &str) -> String {
    match find_top_level(member, ":") {
        Some(at) => format!("{}: {}", member[..at].trim(), canon_type(&member[at + 1..])),
        None => member.trim().to_string(),
    }
}

/// Split on `sep` at bracket depth zero and outside string literals.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut scan = Scanner::new();
    for ch in s.chars() {
        let depth_before = scan.depth;
        scan.feed(ch);
        if ch == sep && depth_before == 0 && scan.depth == 0 && !scan.in_string_before {
            out.push(current.clone());
            current.clear();
            continue;
        }
        current.push(ch);
    }
    out.push(current);
    out
}

/// Byte offset of `needle` at bracket depth zero and outside string literals.
fn find_top_level(s: &str, needle: &str) -> Option<usize> {
    let mut scan = Scanner::new();
    for (index, ch) in s.char_indices() {
        let depth_before = scan.depth;
        let in_string_before = scan.in_string;
        scan.feed(ch);
        if depth_before == 0 && !in_string_before && s[index..].starts_with(needle) {
            return Some(index);
        }
    }
    None
}

/// True when `s` opens with `open`, closes with `close`, and that opener's
/// match is the final character — so the whole expression is one group.
fn is_wrapped(s: &str, open: char, close: char) -> bool {
    if !s.starts_with(open) || !s.ends_with(close) || s.len() < 2 {
        return false;
    }
    let mut scan = Scanner::new();
    for (index, ch) in s.char_indices() {
        scan.feed(ch);
        if scan.depth == 0 && index + ch.len_utf8() < s.len() {
            return false;
        }
    }
    true
}

/// Byte offset of the `<` opening a `Name<...>` whose `>` is the final
/// character, if this atom is a generic reference.
fn generic_open(s: &str) -> Option<usize> {
    if !s.ends_with('>') {
        return None;
    }
    let at = find_top_level(s, "<")?;
    if at == 0 {
        return None;
    }
    let mut scan = Scanner::new();
    for (index, ch) in s.char_indices() {
        scan.feed(ch);
        if index >= at && scan.depth == 0 {
            return if index + ch.len_utf8() == s.len() {
                Some(at)
            } else {
                None
            };
        }
    }
    None
}

/// Bracket/string state while walking a rendered type text. `<`/`>` count as
/// brackets so a generic argument's separators stay nested; the `>` of a `=>`
/// does not, since it closes nothing.
struct Scanner {
    depth: i32,
    in_string: bool,
    quote: char,
    previous: char,
    /// Whether the scanner was inside a string BEFORE the current character —
    /// what a caller testing the current character needs to know.
    in_string_before: bool,
}

impl Scanner {
    fn new() -> Self {
        Self {
            depth: 0,
            in_string: false,
            quote: '\0',
            previous: '\0',
            in_string_before: false,
        }
    }

    fn feed(&mut self, ch: char) {
        self.in_string_before = self.in_string;
        if self.in_string {
            if ch == self.quote && self.previous != '\\' {
                self.in_string = false;
            }
        } else {
            match ch {
                '"' | '\'' | '`' => {
                    self.in_string = true;
                    self.quote = ch;
                }
                '{' | '[' | '(' | '<' => self.depth += 1,
                '}' | ']' | ')' => self.depth -= 1,
                '>' if self.previous != '=' => self.depth -= 1,
                _ => {}
            }
        }
        self.previous = ch;
    }
}

/// P/R/F1 with the contract §1.4 convention: precision is 1.0 when nothing is
/// expected and nothing is found, 0.0 when something is found but nothing matched
/// the empty expectation; recall is 1.0 when nothing is expected.
fn prf(tp: usize, found: usize, expected: usize) -> (f64, f64, f64) {
    let precision = if found == 0 {
        if expected == 0 { 1.0 } else { 0.0 }
    } else {
        tp as f64 / found as f64
    };
    let recall = if expected == 0 {
        1.0
    } else {
        tp as f64 / expected as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (precision, recall, f1)
}

/// Mean and sample stddev (n-1). stddev is 0.0 for n < 2 (contract §1.4).
fn mean_sd(xs: &[f64]) -> (f64, f64) {
    let n = xs.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    if n < 2 {
        return (mean, 0.0);
    }
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    (mean, var.sqrt())
}

// ---------------------------------------------------------------------------
// Label shapes (contract §5). The full S4 scorer reads every field. New since
// the thin slice: the `graphql_operations`/`socket_events` arrays (per repo),
// `_must_not_emit`, `owner`/anchor/resolved-type/`type_state`/`tier` on ops,
// and on `expected-output.json` the `orphans` + `dependency_conflicts` arrays
// plus per-`matches` `protocol`/`tier`/compat fields.
// ---------------------------------------------------------------------------

const TIER_CAPABILITY: &str = "capability";
const TIER_ROADMAP: &str = "roadmap";

#[derive(Debug, Deserialize)]
struct ExpectedRepo {
    #[serde(default)]
    endpoints: Vec<ExpEndpoint>,
    #[serde(default)]
    calls: Vec<ExpCall>,
    /// Non-HTTP producer/consumer ops (GraphQL). `role` partitions them into the
    /// endpoint set (producer) vs call set (consumer); `key` is the canonical
    /// `graphql|<kind>|<field>` form.
    #[serde(default)]
    graphql_operations: Vec<ExpNonHttpOp>,
    /// Non-HTTP producer/consumer ops (Socket.IO), same `role`/`key` convention;
    /// `key` is `socket|<direction>|<event>`. Per the corpus README a *listener*
    /// is the producer (endpoint) and an *emitter* the consumer (call).
    #[serde(default)]
    socket_events: Vec<ExpNonHttpOp>,
    /// Non-HTTP pub/sub ops (Kafka/Redis/NATS). Same role/key convention as
    /// graphql/socket: a *subscriber* is the producer (endpoint), a *publisher*
    /// the consumer (call); `key` is `pubsub|<topic>`. The topic/broker/side/source
    /// fields in the JSON are diagnostic and ignored by serde (unknown-field drop).
    #[serde(default)]
    pubsub_operations: Vec<ExpNonHttpOp>,
    #[serde(default, rename = "_must_not_emit")]
    must_not_emit: Vec<ExpMustNotEmit>,
}

#[derive(Debug, Deserialize)]
struct ExpEndpoint {
    method: String,
    path: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    primary_type_symbol: Option<String>,
    #[serde(default)]
    resolved_type: Option<String>,
    #[serde(default)]
    type_state: Option<String>,
    #[serde(default = "default_tier")]
    tier: String,
}

#[derive(Debug, Deserialize)]
struct ExpCall {
    method: String,
    /// Cross-repo match key path (for row 7); set scoring uses the fuzzy fields.
    #[allow(dead_code)]
    path: String,
    #[serde(default)]
    host_contains: Option<String>,
    #[serde(default)]
    path_contains: Option<String>,
    #[serde(default = "default_tier")]
    tier: String,
}

#[derive(Debug, Deserialize)]
struct ExpNonHttpOp {
    /// `"producer"` (→ endpoint set) or `"consumer"` (→ call set).
    role: String,
    /// Canonical `OperationKey` string: `graphql|<kind>|<field>` or
    /// `socket|<direction>|<event>`.
    key: String,
    #[serde(default)]
    primary_type_symbol: Option<String>,
    #[serde(default)]
    resolved_type: Option<String>,
    #[serde(default)]
    type_state: Option<String>,
    #[serde(default = "default_tier")]
    tier: String,
}

#[derive(Debug, Deserialize)]
struct ExpMustNotEmit {
    /// `"endpoint"` or `"call"` — which projection set this decoy would leak
    /// into. The set matters when a decoy shares `(method, path)` with a legit
    /// op on the OTHER side (corpus-3's intra-repo self-call decoy is a `call`
    /// whose path is also a labelled endpoint). Other values (corpus-2's
    /// non-HTTP `"pubsub"` decoys) match neither HTTP set — documented, not
    /// yet scoreable.
    kind: String,
    /// `"*"` matches any method.
    method: String,
    path: String,
}

fn default_tier() -> String {
    TIER_CAPABILITY.to_string()
}

#[derive(Debug, Deserialize)]
struct ExpectedOutput {
    #[serde(default)]
    matches: Vec<ExpMatch>,
    #[serde(default)]
    orphans: Vec<ExpOrphan>,
    #[serde(default)]
    dependency_conflicts: Vec<ExpDepConflict>,
}

#[derive(Debug, Deserialize)]
struct ExpMatch {
    producer_repo: String,
    producer_key: String,
    consumer_repo: String,
    consumer_key: String,
    /// Expected compat verdict for this edge (row 8). A labelled edge always
    /// carries a non-null verdict (contract §5.2).
    #[serde(default)]
    type_compatible: Option<bool>,
    #[serde(default = "default_tier")]
    tier: String,
}

#[derive(Debug, Deserialize)]
struct ExpOrphan {
    repo: String,
    /// `"producer"` or `"consumer"`.
    side: String,
    /// Canonical `OperationKey` string.
    key: String,
    #[serde(default = "default_tier")]
    tier: String,
}

#[derive(Debug, Deserialize)]
struct ExpDepConflict {
    package: String,
    versions: Vec<String>,
    severity: String,
    #[serde(default = "default_tier")]
    tier: String,
}

// ---------------------------------------------------------------------------
// EvalProjection wire shape. Mirrored locally (not the lib type) so the harness
// reads the projection purely as a wire contract — same stance as the S2 harness.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EvalProjection {
    #[serde(default)]
    endpoints: Vec<EvalOp>,
    #[serde(default)]
    calls: Vec<EvalOp>,
    #[serde(default)]
    cross_repo_matches: Vec<EvalCrossRepoMatch>,
    #[serde(default)]
    dependency_conflicts: Vec<EvalDependencyConflict>,
}

#[derive(Debug, Deserialize, Clone)]
struct EvalOp {
    /// Canonical `OperationKey` string. For GraphQL/socket ops this is the only
    /// place the kind/direction survives (`method` is None, `path` is the bare
    /// field/event), so non-HTTP set scoring keys off this.
    #[serde(default)]
    key: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    handler: Option<String>,
    #[serde(default)]
    type_state: Option<String>,
    #[serde(default)]
    resolved_definition: Option<String>,
    #[serde(default)]
    expanded_definition: Option<String>,
    #[serde(default)]
    primary_type_symbol: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct EvalCrossRepoMatch {
    producer_repo: String,
    producer_key: String,
    consumer_repo: String,
    consumer_key: String,
    #[serde(default)]
    type_compatible: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
struct EvalDependencyConflict {
    package: String,
    versions: Vec<String>,
    severity: String,
}

// ---------------------------------------------------------------------------
// Tier partitioning. Every metric is scored twice — once over the `capability`
// labels, once over the `roadmap` labels — and reported separately (contract §6
// "partitioned by capability vs roadmap, never blended"). The corpus is all
// `capability` today, so the `roadmap` partition is the §1.4 expected-0 perfect
// score until a roadmap label lands.
// ---------------------------------------------------------------------------

const TIERS: [&str; 2] = [TIER_CAPABILITY, TIER_ROADMAP];

// ---------------------------------------------------------------------------
// Pure scoring functions (unit-tested below with synthetic projections).
// ---------------------------------------------------------------------------

/// A normalized op-set member. HTTP ops are keyed `(protocol, METHOD, norm_path)`;
/// GraphQL/socket ops are keyed `(protocol, "", norm_key(canonical))` so the
/// kind/direction the bare `path` drops is preserved. Uniform across protocols.
type OpSetKey = (String, String, String);

/// The expected op-set member for an HTTP endpoint/call label.
fn http_op_key(method: &str, path: &str) -> OpSetKey {
    ("http".to_string(), method.to_uppercase(), norm_path(path))
}

/// The op-set member for a non-HTTP label / projection op, keyed off its
/// canonical operation key (which carries graphql kind / socket direction).
fn nonhttp_op_key(canonical: &str) -> OpSetKey {
    let proto = canonical.split('|').next().unwrap_or("").to_string();
    (proto, String::new(), norm_key(canonical))
}

/// The expected producer op-set for one tier: HTTP `endpoints` + the
/// `role=="producer"` GraphQL/socket ops.
fn expected_endpoint_set(
    repo_expected: &[(String, ExpectedRepo)],
    tier: &str,
) -> HashSet<OpSetKey> {
    let mut set = HashSet::new();
    for (_repo, exp) in repo_expected {
        for e in &exp.endpoints {
            if e.tier == tier {
                set.insert(http_op_key(&e.method, &e.path));
            }
        }
        for op in exp
            .graphql_operations
            .iter()
            .chain(exp.socket_events.iter())
            .chain(exp.pubsub_operations.iter())
        {
            if op.role == "producer" && op.tier == tier {
                set.insert(nonhttp_op_key(&op.key));
            }
        }
    }
    set
}

/// The projection's producer op-set (HTTP + graphql + socket endpoints).
fn projection_endpoint_set(proj: &EvalProjection) -> HashSet<OpSetKey> {
    proj.endpoints.iter().filter_map(op_to_set_key).collect()
}

/// The projection's consumer op-set (HTTP + graphql + socket calls).
fn projection_call_set(proj: &EvalProjection) -> HashSet<OpSetKey> {
    proj.calls.iter().filter_map(op_to_set_key).collect()
}

/// Project one [`EvalOp`] to its set key. HTTP needs method+path; GraphQL/socket
/// key off the canonical `key`.
fn op_to_set_key(o: &EvalOp) -> Option<OpSetKey> {
    if o.protocol == "http" {
        Some(http_op_key(o.method.as_deref()?, o.path.as_deref()?))
    } else {
        Some(nonhttp_op_key(&o.key))
    }
}

/// Attribute an untiered actual `found` set to one `tier`: keep keys this tier
/// expects, plus true false positives (expected by no tier) attributed to the
/// default tier; drop keys another tier expects. This is the uniform tier
/// -partitioning rule (see [`score_matches`]) applied to the op set, so an
/// all-`capability` corpus leaves the `roadmap` partition's `found` empty.
fn attribute_found_to_tier<T: std::hash::Hash + Eq + Clone>(
    found: &HashSet<T>,
    expected_this: &HashSet<T>,
    expected_any: &HashSet<T>,
    expected_other: &HashSet<T>,
    tier: &str,
) -> HashSet<T> {
    found
        .iter()
        .filter(|k| {
            expected_this.contains(*k)
                || (!expected_other.contains(*k)
                    && !expected_any.contains(*k)
                    && tier == TIER_CAPABILITY)
        })
        .cloned()
        .collect()
}

/// Endpoint-set P/R/F1 for one tier (contract §6 row 1).
fn score_endpoint_set(
    repo_expected: &[(String, ExpectedRepo)],
    proj: &EvalProjection,
    tier: &str,
) -> (f64, f64, f64) {
    let expected = expected_endpoint_set(repo_expected, tier);
    let expected_any: HashSet<OpSetKey> = TIERS
        .iter()
        .flat_map(|t| expected_endpoint_set(repo_expected, t))
        .collect();
    let expected_other: HashSet<OpSetKey> = TIERS
        .iter()
        .filter(|t| **t != tier)
        .flat_map(|t| expected_endpoint_set(repo_expected, t))
        .collect();
    let found_all = projection_endpoint_set(proj);
    let found =
        attribute_found_to_tier(&found_all, &expected, &expected_any, &expected_other, tier);
    let tp = expected.intersection(&found).count();
    prf(tp, found.len(), expected.len())
}

/// Call-set P/R/F1 for one tier (contract §6 row 2). HTTP calls use the Tier-A
/// fuzzy match (method == AND host_contains ⊂ key AND path_contains ⊂ path);
/// GraphQL/socket consumers use exact canonical-key set membership. Returns
/// `(precision, recall, f1)` blending both protocol families into one row.
fn score_call_set(
    repo_expected: &[(String, ExpectedRepo)],
    proj: &EvalProjection,
    tier: &str,
) -> (f64, f64, f64) {
    // --- HTTP, fuzzy (Tier-A convention) ---
    let http_calls: Vec<&EvalOp> = proj.calls.iter().filter(|o| o.protocol == "http").collect();
    let expected_http: Vec<&ExpCall> = repo_expected
        .iter()
        .flat_map(|(_r, e)| e.calls.iter())
        .filter(|c| c.tier == tier)
        .collect();
    let expected_http_other: Vec<&ExpCall> = repo_expected
        .iter()
        .flat_map(|(_r, e)| e.calls.iter())
        .filter(|c| c.tier != tier)
        .collect();
    // recall: every expected call matched by ≥1 actual.
    let http_recall_hits = expected_http
        .iter()
        .filter(|ec| http_calls.iter().any(|ac| fuzzy_call_match(ec, ac)))
        .count();
    // precision: every actual call that satisfies ≥1 expected (over this tier).
    let http_precision_hits = http_calls
        .iter()
        .filter(|ac| expected_http.iter().any(|ec| fuzzy_call_match(ec, ac)))
        .count();
    // Tier attribution of the http `found` denominator: an actual claimed by
    // ANOTHER tier's expected call is dropped from this tier's denominator; a
    // true false positive (matches no tier) lands in the default tier. So an
    // all-capability corpus leaves roadmap's http `found` empty.
    let http_found = http_calls
        .iter()
        .filter(|ac| {
            let matches_this = expected_http.iter().any(|ec| fuzzy_call_match(ec, ac));
            let matches_other = expected_http_other
                .iter()
                .any(|ec| fuzzy_call_match(ec, ac));
            matches_this || (!matches_other && tier == TIER_CAPABILITY)
        })
        .count();

    // --- GraphQL/socket consumers, exact canonical key ---
    let nonhttp_expected = |t: &str| -> HashSet<OpSetKey> {
        repo_expected
            .iter()
            .flat_map(|(_r, e)| {
                e.graphql_operations
                    .iter()
                    .chain(e.socket_events.iter())
                    .chain(e.pubsub_operations.iter())
            })
            .filter(|op| op.role == "consumer" && op.tier == t)
            .map(|op| nonhttp_op_key(&op.key))
            .collect()
    };
    let expected_nonhttp = nonhttp_expected(tier);
    let expected_nonhttp_any: HashSet<OpSetKey> =
        TIERS.iter().flat_map(|t| nonhttp_expected(t)).collect();
    let expected_nonhttp_other: HashSet<OpSetKey> = TIERS
        .iter()
        .filter(|t| **t != tier)
        .flat_map(|t| nonhttp_expected(t))
        .collect();
    let found_nonhttp_all: HashSet<OpSetKey> = proj
        .calls
        .iter()
        .filter(|o| o.protocol != "http")
        .map(|o| nonhttp_op_key(&o.key))
        .collect();
    let found_nonhttp = attribute_found_to_tier(
        &found_nonhttp_all,
        &expected_nonhttp,
        &expected_nonhttp_any,
        &expected_nonhttp_other,
        tier,
    );
    let nonhttp_tp = expected_nonhttp.intersection(&found_nonhttp).count();

    // Blend: P over (http precision-eligible actuals + nonhttp actuals),
    // R over (http expected + nonhttp expected).
    let found = http_found + found_nonhttp.len();
    let expected = expected_http.len() + expected_nonhttp.len();
    let tp_precision = http_precision_hits + nonhttp_tp;
    let tp_recall = http_recall_hits + nonhttp_tp;
    let precision = if found == 0 {
        if expected == 0 { 1.0 } else { 0.0 }
    } else {
        tp_precision as f64 / found as f64
    };
    let recall = if expected == 0 {
        1.0
    } else {
        tp_recall as f64 / expected as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (precision, recall, f1)
}

/// Tier-A fuzzy call match: same method, expected `host_contains` is a substring
/// of the actual op's canonical key, and expected `path_contains` is a substring
/// of the actual path. A null `host_contains`/`path_contains` constraint is
/// vacuously satisfied (it is a "no host" / built-in label, e.g. `sendBeacon`).
fn fuzzy_call_match(ec: &ExpCall, ac: &EvalOp) -> bool {
    let method_ok = ac
        .method
        .as_deref()
        .map(|m| m.eq_ignore_ascii_case(&ec.method))
        .unwrap_or(false);
    if !method_ok {
        return false;
    }
    let host_ok = match &ec.host_contains {
        Some(h) => ac.key.contains(h.as_str()),
        None => true,
    };
    let path_ok = match &ec.path_contains {
        Some(p) => ac
            .path
            .as_deref()
            .map(|ap| ap.contains(p.as_str()))
            .unwrap_or(false),
        None => true,
    };
    host_ok && path_ok
}

/// Decoy leakage (contract §6 row 3): the count of actual ops (endpoints +
/// calls) whose normalized `(method, path)` equals a `_must_not_emit` entry
/// of the SAME projection set (`kind: "endpoint"` matches only endpoints,
/// `kind: "call"` only calls) — so a call-decoy sharing a path with a legit
/// endpoint never flags the endpoint. `method: "*"` matches any method.
/// Reported separately, not tier-partitioned (a leak is a leak regardless of
/// which tier the surrounding labels are).
fn score_decoy_leak(repo_expected: &[(String, ExpectedRepo)], proj: &EvalProjection) -> usize {
    let decoys: Vec<&ExpMustNotEmit> = repo_expected
        .iter()
        .flat_map(|(_r, e)| e.must_not_emit.iter())
        .collect();
    let mut leaks = 0usize;
    for (op, set_kind) in proj
        .endpoints
        .iter()
        .map(|o| (o, "endpoint"))
        .chain(proj.calls.iter().map(|o| (o, "call")))
    {
        let (Some(m), Some(p)) = (op.method.as_deref(), op.path.as_deref()) else {
            // Non-HTTP ops have no (method, path); the corpus decoys are all
            // HTTP-keyed, so a non-HTTP op can never match one.
            continue;
        };
        let m_up = m.to_uppercase();
        let p_norm = norm_path(p);
        let is_leak = decoys.iter().any(|d| {
            let kind_ok = d.kind == set_kind;
            let method_ok = d.method == "*" || d.method.eq_ignore_ascii_case(&m_up);
            kind_ok && method_ok && norm_path(&d.path) == p_norm
        });
        if is_leak {
            leaks += 1;
        }
    }
    leaks
}

/// Index the projection's HTTP endpoints by `(METHOD, norm_path)` and its
/// non-HTTP endpoints by canonical key, so owner/type metrics can join expected
/// to actual on the shared key.
fn index_proj_endpoints(proj: &EvalProjection) -> std::collections::HashMap<OpSetKey, &EvalOp> {
    let mut idx = std::collections::HashMap::new();
    for o in &proj.endpoints {
        if let Some(k) = op_to_set_key(o) {
            idx.entry(k).or_insert(o);
        }
    }
    idx
}

/// Group ALL projection ops per key instead of keeping only the first. Fan-in
/// (two repos publishing one pub/sub topic) puts several ops — with genuinely
/// different resolved types — on one key; the type metrics 1:1-claim within the
/// group so each expected slot scores against a distinct actual op rather than
/// whichever op the old `or_insert` happened to keep.
fn group_proj_ops(ops: &[EvalOp]) -> std::collections::HashMap<OpSetKey, Vec<&EvalOp>> {
    let mut idx: std::collections::HashMap<OpSetKey, Vec<&EvalOp>> =
        std::collections::HashMap::new();
    for o in ops {
        if let Some(k) = op_to_set_key(o) {
            idx.entry(k).or_default().push(o);
        }
    }
    idx
}

/// 1:1 claiming join between the expected typed ops of one key and the actual
/// ops grouped on that key. Each expected slot claims the first UNCLAIMED op
/// satisfying `matches`; a claimed op can't satisfy a second slot, so a
/// duplicate projection entry can no longer double-count (and a genuinely
/// missing fan-in op scores as the miss it is). Single-op keys — everything but
/// fan-in — behave exactly as the old first-op join. Returns whether a claim
/// was made.
fn claim_matching_op(
    group: &[&EvalOp],
    claimed: &mut [bool],
    matches: impl Fn(&EvalOp) -> bool,
) -> bool {
    debug_assert_eq!(group.len(), claimed.len());
    for (i, op) in group.iter().enumerate() {
        if !claimed[i] && matches(op) {
            claimed[i] = true;
            return true;
        }
    }
    false
}

/// `(correct, total)` fraction with the convention `(0,0) -> 1.0` (nothing to
/// score is vacuously perfect, matching the §1.4 expected-0 stance).
fn fraction(correct: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        correct as f64 / total as f64
    }
}

/// Owner accuracy (contract §6 row 4): over endpoints present in BOTH the
/// expected and actual sets, the fraction where `expected.owner == actual.handler`.
/// Only HTTP endpoints carry `owner`/`handler`. `null == null` counts correct.
fn score_owner_accuracy(
    repo_expected: &[(String, ExpectedRepo)],
    proj: &EvalProjection,
    tier: &str,
) -> f64 {
    let idx = index_proj_endpoints(proj);
    let mut correct = 0usize;
    let mut total = 0usize;
    for (_r, exp) in repo_expected {
        for e in &exp.endpoints {
            if e.tier != tier {
                continue;
            }
            let key = http_op_key(&e.method, &e.path);
            if let Some(actual) = idx.get(&key) {
                total += 1;
                if e.owner.as_deref() == actual.handler.as_deref() {
                    correct += 1;
                }
            }
        }
    }
    fraction(correct, total)
}

/// Type-anchor accuracy (contract §6 row 5): over ops present in BOTH sets
/// (endpoints + calls, all protocols), the fraction where
/// `expected.primary_type_symbol == actual.primary_type_symbol` (`null==null` ok).
fn score_type_anchor_accuracy(
    repo_expected: &[(String, ExpectedRepo)],
    proj: &EvalProjection,
    tier: &str,
) -> f64 {
    let ep_idx = group_proj_ops(&proj.endpoints);
    let call_idx = group_proj_ops(&proj.calls);
    let mut claimed: std::collections::HashMap<(bool, OpSetKey), Vec<bool>> =
        std::collections::HashMap::new();
    let mut correct = 0usize;
    let mut total = 0usize;
    for_each_expected_typed_op(repo_expected, tier, |key, is_producer, anchor, _rt, _ts| {
        let idx = if is_producer { &ep_idx } else { &call_idx };
        if let Some(group) = idx.get(&key) {
            total += 1;
            let flags = claimed
                .entry((is_producer, key))
                .or_insert_with(|| vec![false; group.len()]);
            if claim_matching_op(group, flags, |op| {
                anchor == op.primary_type_symbol.as_deref()
            }) {
                correct += 1;
            }
        }
    });
    fraction(correct, total)
}

/// Type-resolution correctness (contract §6 row 6): over ops in both sets that
/// carry an expected resolved type, the fraction where `type_state` is exactly
/// equal AND the resolved type is equal up to whitespace and union member order
/// (`canonical_type_text`, carrick#735). The corpus `resolved_type` labels are
/// the fully-inlined structural form (ts-morph `getText()` with NoTruncation),
/// so the actual is `expanded_definition`, falling back to
/// `resolved_definition` (the name-preserving DefinitionResolver form) only
/// when no inlined definition was emitted.
fn score_type_resolution_accuracy(
    repo_expected: &[(String, ExpectedRepo)],
    proj: &EvalProjection,
    tier: &str,
) -> f64 {
    let ep_idx = group_proj_ops(&proj.endpoints);
    let call_idx = group_proj_ops(&proj.calls);
    let mut claimed: std::collections::HashMap<(bool, OpSetKey), Vec<bool>> =
        std::collections::HashMap::new();
    let mut correct = 0usize;
    let mut total = 0usize;
    for_each_expected_typed_op(repo_expected, tier, |key, is_producer, _anchor, rt, ts| {
        let Some(expected_rt) = rt else {
            return; // only score ops with an expected resolved type
        };
        let idx = if is_producer { &ep_idx } else { &call_idx };
        if let Some(group) = idx.get(&key) {
            total += 1;
            let flags = claimed
                .entry((is_producer, key))
                .or_insert_with(|| vec![false; group.len()]);
            if claim_matching_op(group, flags, |actual| {
                let actual_rt = actual
                    .expanded_definition
                    .as_deref()
                    .or(actual.resolved_definition.as_deref());
                let state_ok = ts == actual.type_state.as_deref();
                let rt_ok =
                    actual_rt.map(canonical_type_text) == Some(canonical_type_text(expected_rt));
                state_ok && rt_ok
            }) {
                correct += 1;
            }
        }
    });
    fraction(correct, total)
}

/// Drive a closure over every expected typed op of `tier`: HTTP endpoints (as
/// producers), GraphQL/socket producers (endpoints) and consumers (calls).
/// `is_producer` selects which projection index the caller joins against. Args:
/// `(set_key, is_producer, anchor, resolved_type, type_state)`.
fn for_each_expected_typed_op(
    repo_expected: &[(String, ExpectedRepo)],
    tier: &str,
    mut f: impl FnMut(OpSetKey, bool, Option<&str>, Option<&str>, Option<&str>),
) {
    for (_r, exp) in repo_expected {
        for e in &exp.endpoints {
            if e.tier == tier {
                f(
                    http_op_key(&e.method, &e.path),
                    true,
                    e.primary_type_symbol.as_deref(),
                    e.resolved_type.as_deref(),
                    e.type_state.as_deref(),
                );
            }
        }
        for op in exp
            .graphql_operations
            .iter()
            .chain(exp.socket_events.iter())
            .chain(exp.pubsub_operations.iter())
        {
            if op.tier == tier {
                let is_producer = op.role == "producer";
                f(
                    nonhttp_op_key(&op.key),
                    is_producer,
                    op.primary_type_symbol.as_deref(),
                    op.resolved_type.as_deref(),
                    op.type_state.as_deref(),
                );
            }
        }
    }
}

/// The edge key for the cross-repo match metric (contract §6 row 7):
/// `(producer_repo, norm(producer_key), consumer_repo, norm(consumer_key))`.
fn match_edge_key(
    producer_repo: &str,
    producer_key: &str,
    consumer_repo: &str,
    consumer_key: &str,
) -> (String, String, String, String) {
    (
        producer_repo.to_string(),
        norm_key(producer_key),
        consumer_repo.to_string(),
        norm_key(consumer_key),
    )
}

// ---------------------------------------------------------------------------
// Repo-identity normalization for the edge join (#431).
//
// The projection's `producer_repo`/`consumer_repo` carry the #368
// `service_name ?? repo_name` id: in a monorepo with carrick.json `services[]`
// that is the SERVICE name, while corpus labels (and the harness) know repos by
// their fixture DIR name. The two vocabularies never overlapped on the recorded
// OSS run, so every edge silently missed the join and match F1 collapsed to
// 0.00 with byte-identical keys in the [diag] lines. The single documented join
// identity is the corpus repo dir name; service names fold onto it here, on
// BOTH sides, before any edge metric is scored.
// ---------------------------------------------------------------------------

/// Minimal mirror of `carrick.json`'s service-name shape (the same
/// `name`/`serviceName` aliasing as `src/config.rs`), read purely to build the
/// service→repo fold. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct CarrickConfigFile {
    #[serde(default)]
    services: Vec<CarrickServiceDecl>,
    #[serde(default, rename = "serviceName", alias = "name")]
    service_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CarrickServiceDecl {
    #[serde(default, rename = "name", alias = "serviceName")]
    name: Option<String>,
}

/// Build the `service name → corpus repo dir` fold from each corpus repo's
/// `carrick.json`. A repo without one (or without service names) contributes
/// nothing — its identity is already the dir name. Ambiguity is a hard error:
/// a service name declared by two repos, or colliding with another repo's dir
/// name, would make the fold direction undecidable, so the scorer fails loud
/// rather than join wrongly. An unparseable `carrick.json` (possible on
/// third-party OSS checkouts) is warned and skipped — the scan itself defines
/// how it degrades, not the scorer.
fn load_repo_aliases(repos: &[PathBuf]) -> std::collections::HashMap<String, String> {
    let dir_names: HashSet<String> = repos
        .iter()
        .filter_map(|r| r.file_name().and_then(|s| s.to_str()).map(String::from))
        .collect();
    let mut aliases = std::collections::HashMap::new();
    for repo in repos {
        let dir = repo
            .file_name()
            .and_then(|s| s.to_str())
            .expect("corpus repo has a name")
            .to_string();
        let path = repo.join("carrick.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let cfg: CarrickConfigFile = match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "[eval] WARNING: {} did not parse as carrick.json ({e}) — no service \
                     aliases folded for {dir}",
                    path.display()
                );
                continue;
            }
        };
        // Mirror Config::load_services: a non-empty `services` array supersedes
        // the flat fields.
        let names: Vec<String> = if cfg.services.is_empty() {
            cfg.service_name.into_iter().collect()
        } else {
            cfg.services.into_iter().filter_map(|s| s.name).collect()
        };
        for name in names {
            if name == dir {
                continue; // identity — nothing to fold
            }
            assert!(
                !dir_names.contains(&name),
                "service name {name:?} in {dir}/carrick.json collides with the corpus repo \
                 dir {name:?} — the repo-identity fold (#431) would be ambiguous"
            );
            if let Some(prev) = aliases.insert(name.clone(), dir.clone()) {
                assert!(
                    prev == dir,
                    "service name {name:?} is declared by two corpus repos ({prev} and {dir}) \
                     — the repo-identity fold (#431) would be ambiguous"
                );
            }
        }
    }
    aliases
}

/// Fold one repo identity to the corpus repo dir name: a declared service name
/// maps to its owning repo, anything else (already a repo dir, or unknown) is
/// itself.
fn canon_repo(aliases: &std::collections::HashMap<String, String>, id: &str) -> String {
    aliases.get(id).cloned().unwrap_or_else(|| id.to_string())
}

/// The #431 loud-failure signal: labels AND predictions both non-empty, yet ZERO
/// edges join. With correct extraction that pattern is (near-)impossible except
/// through a join-identity defect, so it must never again hide inside an
/// ordinary-looking 0.00. Computed on the already-folded edge sets; `None` when
/// either side is empty (an empty `found` is a plain recall failure, already
/// visible per-edge in the [diag] MISS lines) or when at least one edge joins.
fn match_join_diagnosis(expected: &[ExpMatch], found: &[EvalCrossRepoMatch]) -> Option<String> {
    if expected.is_empty() || found.is_empty() {
        return None;
    }
    let expected_set: HashSet<_> = expected
        .iter()
        .map(|m| {
            match_edge_key(
                &m.producer_repo,
                &m.producer_key,
                &m.consumer_repo,
                &m.consumer_key,
            )
        })
        .collect();
    let any_join = found.iter().any(|m| {
        expected_set.contains(&match_edge_key(
            &m.producer_repo,
            &m.producer_key,
            &m.consumer_repo,
            &m.consumer_key,
        ))
    });
    if any_join {
        return None;
    }
    let repo_vocab = |producers: Vec<&str>| {
        let mut v: Vec<&str> = producers
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        v.sort_unstable();
        v.join(", ")
    };
    let label_repos = repo_vocab(
        expected
            .iter()
            .flat_map(|m| [m.producer_repo.as_str(), m.consumer_repo.as_str()])
            .collect(),
    );
    let found_repos = repo_vocab(
        found
            .iter()
            .flat_map(|m| [m.producer_repo.as_str(), m.consumer_repo.as_str()])
            .collect(),
    );
    Some(format!(
        "match join produced ZERO joined edges although {} labelled and {} predicted edge(s) \
         exist. Post-fold label repo ids [{label_repos}] vs predicted repo ids [{found_repos}]. \
         A disjoint repo vocabulary means the repo-identity fold (#431) is missing an alias — \
         the match/compat/orphan dimensions are floored by the join, not by extraction.",
        expected.len(),
        found.len(),
    ))
}

/// Cross-repo match P/R/F1 for one tier (contract §6 row 7): exact set equality
/// over the edge key, both sides normalized. Spans HTTP + GraphQL + socket edges.
///
/// **Tier attribution of actuals.** The contract partitions every metric by
/// tier, but the scanner's actual edges carry no tier. An actual edge is
/// attributed to a tier iff it matches an expected edge of that tier, OR it
/// matches no expected edge of ANY tier (a true false positive, attributed to
/// the *default* tier so a spurious edge still dings precision exactly once). So
/// a tier's `found` set excludes actuals claimed by another tier — an
/// all-`capability` corpus then leaves the `roadmap` partition's `found` empty
/// (no roadmap false positives), scoring the §1.4 expected-0 perfect.
fn score_matches(
    expected: &[ExpMatch],
    found: &[EvalCrossRepoMatch],
    tier: &str,
) -> (f64, f64, f64) {
    let edge_key = |m: &ExpMatch| {
        match_edge_key(
            &m.producer_repo,
            &m.producer_key,
            &m.consumer_repo,
            &m.consumer_key,
        )
    };
    let expected_set: HashSet<_> = expected
        .iter()
        .filter(|m| m.tier == tier)
        .map(edge_key)
        .collect();
    // Keys expected by any tier (to detect true false positives) and by some
    // OTHER tier (to exclude them from this tier's `found`).
    let expected_any: HashSet<_> = expected.iter().map(edge_key).collect();
    let expected_other: HashSet<_> = expected
        .iter()
        .filter(|m| m.tier != tier)
        .map(edge_key)
        .collect();
    let found_set: HashSet<_> = found
        .iter()
        .map(|m| {
            match_edge_key(
                &m.producer_repo,
                &m.producer_key,
                &m.consumer_repo,
                &m.consumer_key,
            )
        })
        // Attribute a true false positive (matches no expected edge anywhere) to
        // the default tier; drop edges claimed by another tier.
        .filter(|k| {
            expected_set.contains(k)
                || (!expected_other.contains(k)
                    && (!expected_any.contains(k) && tier == TIER_CAPABILITY))
        })
        .collect();
    let tp = expected_set.intersection(&found_set).count();
    prf(tp, found_set.len(), expected_set.len())
}

/// Compat-verdict accuracy for one tier (contract §6 row 8): over edges present
/// in BOTH expected and actual `matches` (keyed §6 row 7), the fraction where
/// `actual.type_compatible == Some(expected.type_compatible)`. An expected edge
/// with a NULL verdict is compat-**unlabelled** (a partial-label OSS slice) and
/// is skipped — it neither scores nor dings, so a joined-but-unlabelled edge
/// can no longer read as a miss (#431). On the authored corpora every edge
/// carries a verdict (contract §5.2), so this is a no-op there. An actual
/// `None` on a verdict-labelled edge is a miss. The §7 guard is enforced
/// separately by [`compat_guard`] before this is trusted.
fn score_compat_verdict_accuracy(
    expected: &[ExpMatch],
    found: &[EvalCrossRepoMatch],
    tier: &str,
) -> f64 {
    use std::collections::HashMap;
    let actual_by_key: HashMap<(String, String, String, String), Option<bool>> = found
        .iter()
        .map(|m| {
            (
                match_edge_key(
                    &m.producer_repo,
                    &m.producer_key,
                    &m.consumer_repo,
                    &m.consumer_key,
                ),
                m.type_compatible,
            )
        })
        .collect();
    let mut correct = 0usize;
    let mut total = 0usize;
    for m in expected.iter().filter(|m| m.tier == tier) {
        let Some(expected_verdict) = m.type_compatible else {
            continue; // compat-unlabelled edge: not scored on this row
        };
        let key = match_edge_key(
            &m.producer_repo,
            &m.producer_key,
            &m.consumer_repo,
            &m.consumer_key,
        );
        if let Some(actual_compat) = actual_by_key.get(&key) {
            total += 1;
            if *actual_compat == Some(expected_verdict) {
                correct += 1;
            }
        }
    }
    fraction(correct, total)
}

/// The §7 compat-absence guard (load-bearing). If the answer key expects ≥1
/// edge to carry a non-null compat verdict, the actual matches MUST contain ≥1
/// edge with `type_compatible.is_some()` — otherwise compat data was silently
/// absent (the type check never ran) and the scorer must FAIL LOUD rather than
/// score the absence as "all compatible". Returns `Err(msg)` to fail; `Ok(())`
/// to proceed.
fn compat_guard(expected_matches: &[ExpMatch], found: &[EvalCrossRepoMatch]) -> Result<(), String> {
    let expects_verdict = expected_matches.iter().any(|m| m.type_compatible.is_some());
    if !expects_verdict {
        return Ok(());
    }
    let has_verdict = found.iter().any(|m| m.type_compatible.is_some());
    if has_verdict {
        Ok(())
    } else {
        Err(format!(
            "§7 compat-absence guard: {} expected edge(s) carry a non-null compat verdict, but \
             NO actual cross_repo_match has type_compatible.is_some(). Cross-repo type checking \
             was silently skipped (the v2 capture/check never ran). Refusing to score absent \
             compat data as a verdict.",
            expected_matches
                .iter()
                .filter(|m| m.type_compatible.is_some())
                .count()
        ))
    }
}

/// Dependency-conflict P/R/F1 for one tier (contract §6 row 9): exact set
/// equality over `(package, sorted versions, severity)`. Tier attribution of the
/// untiered actuals mirrors [`score_matches`]: an actual conflict is scored under
/// a tier iff it equals a labelled conflict of that tier, or it equals no
/// labelled conflict of ANY tier (a true false positive, attributed to the
/// default tier). So an all-`capability` corpus leaves the `roadmap` partition's
/// `found` empty → §1.4 expected-0 perfect.
fn score_dep_conflicts(
    expected: &[ExpDepConflict],
    found: &[EvalDependencyConflict],
    tier: &str,
) -> (f64, f64, f64) {
    let dep_key = |pkg: &str, versions: &[String], severity: &str| {
        let mut v = versions.to_vec();
        v.sort();
        (pkg.to_string(), v, severity.to_string())
    };
    let expected_set: HashSet<(String, Vec<String>, String)> = expected
        .iter()
        .filter(|d| d.tier == tier)
        .map(|d| dep_key(&d.package, &d.versions, &d.severity))
        .collect();
    let expected_any: HashSet<_> = expected
        .iter()
        .map(|d| dep_key(&d.package, &d.versions, &d.severity))
        .collect();
    let expected_other: HashSet<_> = expected
        .iter()
        .filter(|d| d.tier != tier)
        .map(|d| dep_key(&d.package, &d.versions, &d.severity))
        .collect();
    let found_set: HashSet<(String, Vec<String>, String)> = found
        .iter()
        .map(|d| dep_key(&d.package, &d.versions, &d.severity))
        .filter(|k| {
            expected_set.contains(k)
                || (!expected_other.contains(k)
                    && (!expected_any.contains(k) && tier == TIER_CAPABILITY))
        })
        .collect();
    let tp = expected_set.intersection(&found_set).count();
    prf(tp, found_set.len(), expected_set.len())
}

/// Orphans P/R/F1 for one tier (contract §6 row 10, optional). An expected orphan
/// is a producer/consumer that the matcher SHOULD leave unmatched. The joined
/// projection has no per-repo provenance on its `endpoints`/`calls` (the row-1
/// TODO), so the actual orphan universe is not directly enumerable; the only
/// observable signal is the matched edges. An expected orphan is "confirmed" iff
/// its `(repo, side, norm key)` does NOT appear among the matched edges' endpoints
/// — i.e. the matcher correctly did not match it. A labelled orphan the matcher
/// *did* match is a recall miss (the more dangerous error: a spurious edge). So
/// recall is the metric that bites here; precision is definitionally 1.0 over the
/// observable set. Reported, never asserted. When the projection gains repo
/// provenance on ops, swap to the direct producer/consumer minus matched-edge
/// difference for a true precision too.
fn score_orphans(
    expected: &[ExpOrphan],
    matches: &[EvalCrossRepoMatch],
    tier: &str,
) -> (f64, f64, f64) {
    let matched: HashSet<(String, String, String)> = matches
        .iter()
        .flat_map(|m| {
            [
                (
                    m.producer_repo.clone(),
                    "producer".to_string(),
                    norm_key(&m.producer_key),
                ),
                (
                    m.consumer_repo.clone(),
                    "consumer".to_string(),
                    norm_key(&m.consumer_key),
                ),
            ]
        })
        .collect();
    let expected_orphans: HashSet<(String, String, String)> = expected
        .iter()
        .filter(|o| o.tier == tier)
        .map(|o| (o.repo.clone(), o.side.clone(), norm_key(&o.key)))
        .collect();
    let tp = expected_orphans
        .iter()
        .filter(|o| !matched.contains(*o))
        .count();
    prf(tp, tp, expected_orphans.len())
}

/// One run's full metric vector, per tier.
#[derive(Clone)]
struct TierScore {
    ep_prf: (f64, f64, f64),
    call_prf: (f64, f64, f64),
    match_prf: (f64, f64, f64),
    owner: f64,
    type_anchor: f64,
    type_resolution: f64,
    compat_verdict: f64,
    dep_prf: (f64, f64, f64),
    orphan_prf: (f64, f64, f64),
}

#[derive(Clone)]
struct RunScore {
    /// Keyed by tier name (`capability` / `roadmap`).
    by_tier: std::collections::HashMap<String, TierScore>,
    /// Decoy leakage (not tier-partitioned).
    decoy_leak: usize,
    /// The §7 compat-presence verdict for this run. `Ok(())` = the answer key
    /// expected no verdict OR ≥1 actual edge carried a non-null `type_compatible`
    /// (compat reached). `Err(msg)` = the answer key expected ≥1 verdict but NO
    /// actual edge carried one (compat silently absent). When `Err`, the
    /// compat-verdict metric in `by_tier` is meaningless (0/0 → 0.0); the caller
    /// marks it **unscored** rather than reading the absence as a fake
    /// "compatible", and reds the run at the very end. This preserves the §7
    /// contract intent ("never score absent compat as a verdict") without
    /// blocking the rest of the vector from printing.
    compat: Result<(), String>,
    /// The #431 zero-join diagnosis: `Some(msg)` when labels AND predictions
    /// both exist but ZERO edges joined (post-fold) — the silent-collapse
    /// signature this bug class hid behind. Printed loud by the caller;
    /// report-only.
    match_zero_join: Option<String>,
}

/// Score one joined projection against the corpus labels. Computes the FULL
/// per-metric vector unconditionally, and records the §7 compat-presence verdict
/// in [`RunScore::compat`] rather than failing loud here. Decoupling the §7 check
/// from scoring is deliberate (#226): when compat is silently absent the rest of
/// the vector (owner / anchor / resolution / deps / decoys) is still valid and
/// must still print — only the compat-verdict number is meaningless, so the
/// caller marks it unscored and reds the run at the end.
fn score_corpus(
    proj: &EvalProjection,
    repo_expected: &[(String, ExpectedRepo)],
    expected_output: &ExpectedOutput,
    repo_aliases: &std::collections::HashMap<String, String>,
) -> RunScore {
    // #431: fold every repo identity — label side AND projection side — to the
    // corpus repo dir name before ANY edge metric (match / compat / orphans) is
    // scored. One choke point so no per-run path can forget the fold.
    let expected_matches: Vec<ExpMatch> = expected_output
        .matches
        .iter()
        .map(|m| ExpMatch {
            producer_repo: canon_repo(repo_aliases, &m.producer_repo),
            producer_key: m.producer_key.clone(),
            consumer_repo: canon_repo(repo_aliases, &m.consumer_repo),
            consumer_key: m.consumer_key.clone(),
            type_compatible: m.type_compatible,
            tier: m.tier.clone(),
        })
        .collect();
    let expected_orphans: Vec<ExpOrphan> = expected_output
        .orphans
        .iter()
        .map(|o| ExpOrphan {
            repo: canon_repo(repo_aliases, &o.repo),
            side: o.side.clone(),
            key: o.key.clone(),
            tier: o.tier.clone(),
        })
        .collect();
    let found_matches: Vec<EvalCrossRepoMatch> = proj
        .cross_repo_matches
        .iter()
        .map(|m| EvalCrossRepoMatch {
            producer_repo: canon_repo(repo_aliases, &m.producer_repo),
            consumer_repo: canon_repo(repo_aliases, &m.consumer_repo),
            ..m.clone()
        })
        .collect();

    let compat = compat_guard(&expected_matches, &found_matches);
    let match_zero_join = match_join_diagnosis(&expected_matches, &found_matches);
    let mut by_tier = std::collections::HashMap::new();
    for tier in TIERS {
        by_tier.insert(
            tier.to_string(),
            TierScore {
                ep_prf: score_endpoint_set(repo_expected, proj, tier),
                call_prf: score_call_set(repo_expected, proj, tier),
                match_prf: score_matches(&expected_matches, &found_matches, tier),
                owner: score_owner_accuracy(repo_expected, proj, tier),
                type_anchor: score_type_anchor_accuracy(repo_expected, proj, tier),
                type_resolution: score_type_resolution_accuracy(repo_expected, proj, tier),
                compat_verdict: score_compat_verdict_accuracy(
                    &expected_matches,
                    &found_matches,
                    tier,
                ),
                dep_prf: score_dep_conflicts(
                    &expected_output.dependency_conflicts,
                    &proj.dependency_conflicts,
                    tier,
                ),
                orphan_prf: score_orphans(&expected_orphans, &found_matches, tier),
            },
        );
    }
    RunScore {
        by_tier,
        decoy_leak: score_decoy_leak(repo_expected, proj),
        compat,
        match_zero_join,
    }
}

// ---------------------------------------------------------------------------
// EvalRunRecord — the longitudinal store row. Same schema as Tier-A's record
// (the cross-repo fields were added S6); the full S4 scorer populates the whole
// vector. The capability-tier numbers ride the record's scalar/Option columns
// (the corpus is all-capability today); roadmap is reported in-console.
// ---------------------------------------------------------------------------

const RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvalRunRecord {
    schema_version: u32,
    ts_unix: u64,
    scanner_version: String,
    carrick_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github_run_id: Option<String>,
    fixture: String,
    runs_requested: usize,
    runs_effective: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_hash: Option<String>,
    // Endpoint-set metric (contract §6 row 1) rides the `ep_*` columns.
    ep_precision_mean: f64,
    ep_precision_sd: f64,
    ep_recall_mean: f64,
    ep_recall_sd: f64,
    ep_f1_mean: f64,
    ep_f1_sd: f64,
    ep_pass_at_k: f64,
    ep_pass_pow_k: f64,
    // Call-set metric (contract §6 row 2) rides the `call_*` columns.
    call_precision_mean: f64,
    call_precision_sd: f64,
    call_recall_mean: f64,
    call_recall_sd: f64,
    call_f1_mean: f64,
    call_f1_sd: f64,
    call_pass_at_k: f64,
    call_pass_pow_k: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    // --- cross-repo facets (the full S4 vector) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corpus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_precision_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_precision_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_recall_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_recall_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_f1_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    match_f1_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    type_anchor_accuracy_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    type_anchor_accuracy_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    type_resolution_accuracy_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    type_resolution_accuracy_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compat_verdict_accuracy_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compat_verdict_accuracy_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_accuracy_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_accuracy_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decoy_leak_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decoy_leak_sd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dep_f1_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dep_f1_sd: Option<f64>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus_dir() -> PathBuf {
    manifest_dir().join("tests/fixtures").join(corpus_name())
}

/// The corpus repos: every immediate subdirectory of the corpus dir that holds a
/// `package.json`. Crucially this lists the **3 top-level repos** and does NOT
/// descend into `orders-monorepo/packages/*` (those package.jsons are not
/// immediate children of the corpus dir). Sorted for deterministic Phase-A order.
fn discover_repos(corpus: &Path) -> Vec<PathBuf> {
    let mut repos: Vec<PathBuf> = std::fs::read_dir(corpus)
        .unwrap_or_else(|e| panic!("failed to read corpus dir {}: {e}", corpus.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.join("package.json").exists())
        .collect();
    repos.sort();
    repos
}

// ---------------------------------------------------------------------------
// Live env policy (the #211 lesson, made distinct for LIVE mode).
// ---------------------------------------------------------------------------

/// Strip the ambient CI repo *identity* so each corpus repo's name resolves to
/// its own directory, but **keep the OIDC token env** so the live scanner can
/// mint its keyless cloud auth for the real LLM call.
///
/// This is the LIVE counterpart to the S2 mock harness's `strip_ci_env`, which
/// also removes `ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN`. We must NOT remove those
/// here — without them the scanner cannot authenticate and every scan dies.
///
/// `GITHUB_REPOSITORY` is the load-bearing one: `get_repository_name`
/// (`src/utils.rs`) prefers it over the scanned path, so leaving it set collapses
/// every corpus repo's identity to `"carrick"` (the runner's repo) and they
/// clobber each other down to a single cache file (#211). `GITHUB_REF` /
/// `GITHUB_EVENT_NAME` are stripped too so `should_upload_data()` is decided by
/// `CARRICK_LOCAL_STORAGE_DIR` (→ true, local cache) rather than the runner's
/// PR/branch context.
fn strip_ci_identity_keep_oidc(cmd: &mut Command) -> &mut Command {
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        // NOTE: ACTIONS_ID_TOKEN_REQUEST_URL / _TOKEN are deliberately KEPT —
        // the live scanner needs them to mint the OIDC token for real LLM auth.
    ] {
        cmd.env_remove(var);
    }
    cmd
}

/// Phase A (live): scan one repo in isolation with the real LLM and persist its
/// `CloudRepoData` to the shared cache. `CARRICK_LOCAL_STORAGE_ISOLATE=1` forces
/// `download_all_repo_data` to return empty (no sibling/cloud leak); the
/// `CARRICK_LOCAL_STORAGE_DIR` presence flips `should_upload_data()` to true so
/// the upload lands in the LOCAL cache, never the real cloud. `CARRICK_OUTPUT_JSON`
/// is deliberately unset (it would skip the upload).
fn phase_a_live(bin: &Path, repo: &Path, cache_dir: &Path) {
    let mut cmd = Command::new(bin);
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache_dir)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env_remove("CARRICK_OUTPUT_JSON")
        // No CARRICK_MOCK_ALL: this is the LIVE path (real LLM).
        .env_remove("CARRICK_MOCK_ALL");
    strip_ci_identity_keep_oidc(&mut cmd);
    let output = cmd.output().unwrap_or_else(|e| {
        panic!(
            "failed to spawn carrick (Phase A live) for {}: {e}",
            repo.display()
        )
    });
    assert!(
        output.status.success(),
        "Phase A live scan of {} exited non-zero:\n{}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Phase A (mock): the offline plumbing-smoke variant. Identical wiring to the
/// live path except `CARRICK_MOCK_ALL=1` substitutes the heuristic mock for the
/// LLM, so it runs without OIDC under plain `cargo test`. Used only by the
/// `#[ignore]`d mock-smoke test.
fn phase_a_mock(bin: &Path, repo: &Path, cache_dir: &Path) {
    let mut cmd = Command::new(bin);
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache_dir)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env_remove("CARRICK_OUTPUT_JSON");
    // Mock mode needs no cloud auth, so strip OIDC too (matches S2's strip_ci_env).
    strip_ci_identity_keep_oidc(&mut cmd);
    cmd.env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN");
    let output = cmd.output().unwrap_or_else(|e| {
        panic!(
            "failed to spawn carrick (Phase A mock) for {}: {e}",
            repo.display()
        )
    });
    assert!(
        output.status.success(),
        "Phase A mock scan of {} exited non-zero:\n{}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Assert Phase A persisted *something* for `repo` to the cache. A single-service
/// repo writes `<repo>.json`; a multi-service repo (the monorepo declares
/// `orders-pkg` + `gateway` in its `carrick.json`) writes one
/// `<repo>__<service>.json` per service (LocalDirStorage's multi-service keying).
/// So we assert at least one cache file whose stem is `<repo>` or `<repo>__*`.
fn assert_phase_a_persisted(repo: &Path, cache_dir: &Path) {
    let repo_name = repo
        .file_name()
        .and_then(|s| s.to_str())
        .expect("corpus repo has a name");
    let any = std::fs::read_dir(cache_dir)
        .expect("read cache dir")
        .filter_map(|e| e.ok())
        .any(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                return false;
            }
            match p.file_stem().and_then(|s| s.to_str()) {
                Some(stem) => stem == repo_name || stem.starts_with(&format!("{repo_name}__")),
                None => false,
            }
        });
    assert!(
        any,
        "Phase A persisted no cache file for {repo_name} (expected {repo_name}.json or \
         {repo_name}__<service>.json) in {}",
        cache_dir.display()
    );
}

/// Phase B: join the cached repos and emit the merged projection. The type
/// sidecar is auto-discovered by the binary (contract §7 seam), so cross-repo
/// type checking *runs* and the compat-verdict metric is scored over real data.
/// Fails loud if type checking was silently skipped (no sidecar), per §7.
fn phase_b(bin: &Path, repo: &Path, cache_dir: &Path, mock: bool) -> EvalProjection {
    let mut cmd = Command::new(bin);
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache_dir)
        .env("CARRICK_OUTPUT_JSON", "1")
        .env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
    if mock {
        cmd.env("CARRICK_MOCK_ALL", "1");
    } else {
        cmd.env_remove("CARRICK_MOCK_ALL");
    }
    strip_ci_identity_keep_oidc(&mut cmd);
    if mock {
        cmd.env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
            .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN");
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn carrick (Phase B): {e}"));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "Phase B scan exited non-zero:\n{stderr}"
    );
    // Contract §7 seam: type checking only runs when the sidecar is available.
    // Fail loud if it was silently skipped — the compat-verdict metric must
    // never be built on silently-absent compat data.
    assert!(
        !stderr.contains("Skipping type checking"),
        "the type sidecar was not found, so cross-repo type checking was silently \
         skipped. Ensure src/sidecar is built (npm ci && npm run build).\n{stderr}"
    );
    let stdout = String::from_utf8(output.stdout).expect("Phase B stdout was not UTF-8");
    parse_projection(&stdout).unwrap_or_else(|| {
        panic!("Phase B stdout was not a valid EvalProjection:\n{stdout}");
    })
}

/// Tolerate log noise around the JSON by slicing from the first `{` to the last `}`.
fn parse_projection(stdout: &str) -> Option<EvalProjection> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    serde_json::from_str(stdout.get(start..=end)?).ok()
}

/// Load every repo's `expected.json`, keyed by repo dir name.
fn load_repo_expected(repos: &[PathBuf]) -> Vec<(String, ExpectedRepo)> {
    repos
        .iter()
        .map(|repo| {
            let name = repo
                .file_name()
                .and_then(|s| s.to_str())
                .expect("repo has a name")
                .to_string();
            let text = std::fs::read_to_string(repo.join("expected.json"))
                .unwrap_or_else(|e| panic!("read expected.json for {name}: {e}"));
            let exp: ExpectedRepo = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("parse expected.json for {name}: {e}"));
            (name, exp)
        })
        .collect()
}

fn load_expected_output(corpus: &Path) -> ExpectedOutput {
    let text = std::fs::read_to_string(corpus.join("expected-output.json"))
        .expect("read expected-output.json");
    serde_json::from_str(&text).expect("parse expected-output.json")
}

/// Run the full two-phase loop and return the joined projection. `mock` selects
/// the offline plumbing-smoke path (heuristic LLM, no OIDC) vs the live path.
fn run_two_phase(bin: &Path, corpus: &Path, mock: bool) -> EvalProjection {
    let repos = discover_repos(corpus);
    assert!(
        !repos.is_empty(),
        "expected at least one top-level corpus repo, found {} in {} — discover_repos must \
         NOT descend into nested workspace packages (e.g. orders-monorepo/packages/*)",
        repos.len(),
        corpus.display()
    );
    let cache = tempfile::tempdir().expect("failed to create temp cache dir");
    for repo in &repos {
        if mock {
            phase_a_mock(bin, repo, cache.path());
        } else {
            phase_a_live(bin, repo, cache.path());
        }
        assert_phase_a_persisted(repo, cache.path());
    }
    phase_b(bin, &repos[0], cache.path(), mock)
}

fn emit_record(record: &EvalRunRecord) {
    let line = serde_json::to_string(record).expect("serialise EvalRunRecord");
    let out_dir = manifest_dir().join("target/eval-runs");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("[eval] could not create {}: {e}", out_dir.display());
    } else {
        let path = out_dir.join(format!("xrepo-run-{}.jsonl", record.ts_unix));
        match std::fs::File::create(&path).and_then(|mut f| writeln!(f, "{line}")) {
            Ok(()) => println!("\n[eval] wrote 1 record to {}", path.display()),
            Err(e) => eprintln!("[eval] could not write {}: {e}", path.display()),
        }
    }
    println!("=== EVAL JSONL BEGIN ===");
    println!("{line}");
    println!("=== EVAL JSONL END ===");
}

/// Mean±sd of one extracted f64 across the per-run scores, for a given tier.
fn agg(scores: &[RunScore], tier: &str, pick: impl Fn(&TierScore) -> f64) -> (f64, f64) {
    let xs: Vec<f64> = scores
        .iter()
        .filter_map(|s| s.by_tier.get(tier).map(&pick))
        .collect();
    mean_sd(&xs)
}

/// Print the full metric vector for one tier (report-only).
/// `compat_unscored_reason` marks the compat-verdict row UNSCORED with its
/// cause: either the §7 absence (#226 — no run produced a non-null verdict, so
/// the aggregate would read absent data as "0% compatible") or an unlabelled
/// slice (#431 — no labelled edge carries a verdict, so the vacuous 1.00 would
/// read total abstention as a perfect score). Either way the number is never
/// printed as if it were real.
/// A single "generalised correctness" percentage for a run, so progress across
/// runs is easy to track at a glance. It is the **unweighted mean of the
/// fractional accuracy dimensions** — endpoint/call/xrepo-match F1, type-anchor,
/// type-resolution, dependency F1, and compat-verdict *when it was scored* (§7).
///
/// Deliberately coarse: the dimensions matter differently and the type
/// dimensions are currently floored by structural gaps (#245 non-HTTP type
/// extraction, #246 expanded-definition inlining), so this number is a trend
/// indicator, not a contract. Decoy leakage is intentionally excluded — it is a
/// count (a hard quality failure), not a fraction to average away.
fn overall_correctness(cap: &TierScore, compat_scored: bool) -> f64 {
    let mut dims = vec![
        cap.ep_prf.2,
        cap.call_prf.2,
        cap.match_prf.2,
        cap.type_anchor,
        cap.type_resolution,
        cap.dep_prf.2,
    ];
    if compat_scored {
        dims.push(cap.compat_verdict);
    }
    dims.iter().sum::<f64>() / dims.len() as f64
}

/// Whether a run dipped and therefore needs its per-edge diagnostics block: a
/// scored dimension below full marks (surfaced as OVERALL < 100%, exact because
/// a mean of exact 1.0s is exactly 1.0), a decoy leak, or a tripped §7
/// compat-absence guard. A compat dimension that is merely **unlabelled**
/// (`compat_scored == false` with the guard clean — a partial-label OSS slice)
/// is a property of the slice, not a dip, so it does not fire the block by
/// itself. The caller additionally always prints run 1 as the healthy
/// reference to diff a dip against (`run_idx == 1 || run_needs_diag(..)`);
/// run indexing is deliberately not this predicate's concern. Keeps CI output
/// sane — a clean N-run eval prints one block, while a dip run captures its own
/// failing signature in the log it happened in instead of forcing a manual
/// re-root-cause next session.
fn run_needs_diag(
    cap: &TierScore,
    compat_guard_failed: bool,
    compat_scored: bool,
    decoy_leak: usize,
) -> bool {
    compat_guard_failed || decoy_leak > 0 || overall_correctness(cap, compat_scored) < 1.0
}

fn report_tier(
    scores: &[RunScore],
    tier: &str,
    n: usize,
    runs_n: usize,
    compat_unscored_reason: Option<&str>,
) {
    let (ep_p, ep_p_sd) = agg(scores, tier, |t| t.ep_prf.0);
    let (ep_r, ep_r_sd) = agg(scores, tier, |t| t.ep_prf.1);
    let (ep_f, ep_f_sd) = agg(scores, tier, |t| t.ep_prf.2);
    let (cl_p, cl_p_sd) = agg(scores, tier, |t| t.call_prf.0);
    let (cl_r, cl_r_sd) = agg(scores, tier, |t| t.call_prf.1);
    let (cl_f, cl_f_sd) = agg(scores, tier, |t| t.call_prf.2);
    let (m_p, m_p_sd) = agg(scores, tier, |t| t.match_prf.0);
    let (m_r, m_r_sd) = agg(scores, tier, |t| t.match_prf.1);
    let (m_f, m_f_sd) = agg(scores, tier, |t| t.match_prf.2);
    let (own, own_sd) = agg(scores, tier, |t| t.owner);
    let (anc, anc_sd) = agg(scores, tier, |t| t.type_anchor);
    let (res, res_sd) = agg(scores, tier, |t| t.type_resolution);
    let (cmp, cmp_sd) = agg(scores, tier, |t| t.compat_verdict);
    let (dp_f, dp_f_sd) = agg(scores, tier, |t| t.dep_prf.2);
    let (orph_f, orph_f_sd) = agg(scores, tier, |t| t.orphan_prf.2);

    // Overall = mean of the fractional dimension means (compat only when scored),
    // mirroring `overall_correctness` for a per-tier headline.
    let mut dim_means = vec![ep_f, cl_f, m_f, anc, res, dp_f];
    if compat_unscored_reason.is_none() {
        dim_means.push(cmp);
    }
    let overall = dim_means.iter().sum::<f64>() / dim_means.len() as f64;

    println!("\n{} — tier={tier} (n={n}/{runs_n})", corpus_name());
    println!(
        "  OVERALL correctness   {:.0}%  (unweighted mean of {} dims; decoy_leak excluded)",
        overall * 100.0,
        dim_means.len()
    );
    println!(
        "  endpoint-set   P {ep_p:.2}±{ep_p_sd:.2}  R {ep_r:.2}±{ep_r_sd:.2}  F1 {ep_f:.2}±{ep_f_sd:.2}"
    );
    println!(
        "  call-set       P {cl_p:.2}±{cl_p_sd:.2}  R {cl_r:.2}±{cl_r_sd:.2}  F1 {cl_f:.2}±{cl_f_sd:.2}"
    );
    println!(
        "  xrepo match    P {m_p:.2}±{m_p_sd:.2}  R {m_r:.2}±{m_r_sd:.2}  F1 {m_f:.2}±{m_f_sd:.2}"
    );
    println!("  owner accuracy        {own:.2}±{own_sd:.2}");
    println!("  type-anchor accuracy  {anc:.2}±{anc_sd:.2}");
    println!("  type-resolution acc   {res:.2}±{res_sd:.2}");
    match compat_unscored_reason {
        Some(reason) => println!("  compat-verdict acc    UNSCORED ({reason})"),
        None => println!("  compat-verdict acc    {cmp:.2}±{cmp_sd:.2}"),
    }
    println!("  dependency F1         {dp_f:.2}±{dp_f_sd:.2}");
    println!("  orphans F1            {orph_f:.2}±{orph_f_sd:.2}");
}

/// The live cross-repo scorer. Gated behind `#[ignore]` AND `CARRICK_EVAL_LIVE=1`
/// so a plain `cargo test` (or `cargo test --test eval_xrepo`) never triggers a
/// costly real-LLM scan. Run it explicitly:
///
/// ```text
/// CARRICK_EVAL_LIVE=1 cargo test --release --test eval_xrepo -- --ignored \
///     xrepo_live_scorer --test-threads=1 --nocapture
/// ```
#[test]
#[ignore = "live: real LLM scan, costs money — run via eval-xrepo.yml workflow_dispatch"]
fn xrepo_live_scorer() {
    if std::env::var("CARRICK_EVAL_LIVE").map(|v| v.is_empty()) != Ok(false) {
        eprintln!("[eval] CARRICK_EVAL_LIVE not set — skipping live cross-repo scorer.");
        return;
    }
    if std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").is_err() {
        eprintln!(
            "[eval] GitHub Actions OIDC unavailable — skipping live cross-repo scorer \
             (run in CI with `permissions: id-token: write`)."
        );
        return;
    }

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_carrick"));
    let corpus = corpus_dir();
    assert!(corpus.is_dir(), "corpus dir missing: {}", corpus.display());

    let runs_n: usize = std::env::var("CARRICK_EVAL_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&v| v >= 1)
        .unwrap_or(DEFAULT_RUNS);

    let repos = discover_repos(&corpus);
    let repo_expected = load_repo_expected(&repos);
    let expected_output = load_expected_output(&corpus);
    let repo_aliases = load_repo_aliases(&repos);
    // Whether ANY labelled edge carries a compat verdict. When none does (a
    // partial-label OSS slice), the compat dimension is UNSCORED — a vacuous
    // 1.00 there would make total abstention indistinguishable from a pipeline
    // that ran and passed (#431).
    let compat_labeled = expected_output
        .matches
        .iter()
        .any(|m| m.type_compatible.is_some());

    println!(
        "\n=== Cross-repo live scorer ({}, N={runs_n}) — full S4 vector ===",
        corpus_name()
    );
    println!("(report-only monitor; the only fail-loud is the §7 compat-absence guard)\n");
    if !repo_aliases.is_empty() {
        let mut folds: Vec<String> = repo_aliases
            .iter()
            .map(|(service, repo)| format!("{service} -> {repo}"))
            .collect();
        folds.sort();
        eprintln!(
            "[diag] repo-identity fold (#431, carrick.json service -> corpus repo dir): {}",
            folds.join(", ")
        );
    }

    let mut scores: Vec<RunScore> = Vec::new();
    for run_idx in 1..=runs_n {
        let proj = run_two_phase(&bin, &corpus, false);
        // Score the FULL vector unconditionally (#226). The §7 compat-presence
        // check no longer panics inside the scorer: it rides `score.compat`, so
        // owner / anchor / resolution / deps / decoys still print even when
        // cross-repo type checking produced no verdict. When compat is absent the
        // compat-verdict number is meaningless — print `UNSCORED` (never a fake
        // "compat 1.00"), and red the whole run at the end. Scored BEFORE the
        // diag block so the diag trigger below can see this run's numbers.
        let score = score_corpus(&proj, &repo_expected, &expected_output, &repo_aliases);
        let compat_guard_failed = score.compat.is_err();
        let compat_scored = compat_labeled && !compat_guard_failed;
        let cap = score.by_tier.get(TIER_CAPABILITY).expect("capability tier");
        // #431 loud-failure signal: labels + predictions both present, zero
        // joins. This exact signature previously printed as an unremarkable
        // 0.00, so it gets its own unmissable line.
        if let Some(msg) = &score.match_zero_join {
            eprintln!("[warn] run {run_idx}: {msg}");
        }
        // Extraction diagnostic (report-only; run 1 + any dipped run). A
        // cross-repo match metric of 0 (or endpoint recall < 1) is otherwise
        // opaque: surface the ENDPOINT SET, the calls, and the edges so a miss is
        // attributable — which producer endpoint failed to extract, what URL the
        // consumer called, and which repo identities the edges carry. Run 1 gives
        // the healthy reference; a dipped run (`run_needs_diag`) prints its own
        // block so recurring dip signatures land in the CI log instead of needing
        // a manual re-root-cause. Passing runs after run 1 stay silent.
        if run_idx == 1 || run_needs_diag(cap, compat_guard_failed, compat_scored, score.decoy_leak)
        {
            let found_eps = projection_endpoint_set(&proj);
            let expected_eps = expected_endpoint_set(&repo_expected, TIER_CAPABILITY);
            eprintln!(
                "[diag] run {run_idx} projection: {} endpoints, {} calls, {} cross_repo_matches, \
                 {} dependency_conflicts",
                proj.endpoints.len(),
                proj.calls.len(),
                proj.cross_repo_matches.len(),
                proj.dependency_conflicts.len()
            );
            eprintln!(
                "[diag] endpoint set: {} expected (capability), {} found",
                expected_eps.len(),
                found_eps.len()
            );
            for e in &expected_eps {
                let mark = if found_eps.contains(e) { "ok " } else { "MISS" };
                eprintln!("[diag]   endpoint[{mark}] {}|{}|{}", e.0, e.1, e.2);
            }
            for e in &found_eps {
                if !expected_eps.contains(e) {
                    eprintln!("[diag]   endpoint[XTRA] {}|{}|{}", e.0, e.1, e.2);
                }
            }
            let found_calls = projection_call_set(&proj);
            eprintln!(
                "[diag] call set: {} found (distinct keys)",
                found_calls.len()
            );
            for c in &proj.calls {
                eprintln!(
                    "[diag]   call {} {} (key {})",
                    c.method.clone().unwrap_or_default(),
                    c.path.clone().unwrap_or_default(),
                    c.key
                );
            }
            for m in &proj.cross_repo_matches {
                eprintln!(
                    "[diag]   edge {}|{} -> {}|{} (compat={:?})",
                    m.producer_repo,
                    m.producer_key,
                    m.consumer_repo,
                    m.consumer_key,
                    m.type_compatible
                );
            }
            // Per-op type pipeline state (#226 disambiguator). The compat-verdict
            // metric and the §7 guard depend on the cross-repo check; the
            // type_state / resolved_definition fields below are independent —
            // they ride the per-repo sidecar manifest, not the check. So if these
            // show resolved types but every edge above is `compat=None`, inference
            // WAS reached and the gap is the cross-repo check / verdict step
            // (cause (b)); if these are empty too, the per-repo type pipeline
            // never resolved (cause (a)).
            let resolved_ops = proj
                .endpoints
                .iter()
                .chain(proj.calls.iter())
                .filter(|o| o.resolved_definition.is_some() || o.expanded_definition.is_some())
                .count();
            eprintln!(
                "[diag] type pipeline: {} of {} ops carry a resolved_definition / \
                 expanded_definition (>0 ⇒ inference reached ⇒ a compat=None gap is \
                 the check/verdict step, not the type pipeline)",
                resolved_ops,
                proj.endpoints.len() + proj.calls.len(),
            );
            for o in proj.endpoints.iter().chain(proj.calls.iter()) {
                let resolved = o
                    .resolved_definition
                    .as_deref()
                    .or(o.expanded_definition.as_deref());
                eprintln!(
                    "[diag]   op {} type_state={:?} resolved={} {}",
                    o.key,
                    o.type_state.as_deref().unwrap_or("(none)"),
                    if resolved.is_some() { "yes" } else { "no " },
                    resolved
                        .map(|r| format!("-> {}", collapse_ws(r)))
                        .unwrap_or_default(),
                );
            }
            // Per-op anchor: expected vs actual primary_type_symbol (#240). Makes
            // the anchor metric attributable — for each expected-typed op, show
            // whether the real symbol was extracted (`ok`), fell back to the
            // hashed `type_alias` / a different symbol (`MISS` with the actual
            // value), or has no projection entry at all (`actual=None`, the
            // non-HTTP type-pipeline gap, #245).
            let ep_idx = group_proj_ops(&proj.endpoints);
            let call_idx = group_proj_ops(&proj.calls);
            let mut diag_claimed: std::collections::HashMap<(bool, OpSetKey), Vec<bool>> =
                std::collections::HashMap::new();
            eprintln!("[diag] anchor (expected primary_type_symbol vs actual):");
            for_each_expected_typed_op(
                &repo_expected,
                TIER_CAPABILITY,
                |key, is_producer, anchor, _rt, _ts| {
                    let idx = if is_producer { &ep_idx } else { &call_idx };
                    let group = idx.get(&key);
                    // Mirror the metric's 1:1 claiming so the diag shows what the
                    // score saw. Distinguish "no projection entry at all" (the
                    // #245 non-HTTP type-pipeline gap) from "entry present but
                    // anchor null/wrong" so a miss is attributable rather than
                    // conflated. `NOENT` = the op isn't in the projection;
                    // `MISS` = it is, but no unclaimed op carries the expected
                    // anchor.
                    let (mark, actual) = match group {
                        None => ("NOENT", None),
                        Some(group) => {
                            let flags = diag_claimed
                                .entry((is_producer, key.clone()))
                                .or_insert_with(|| vec![false; group.len()]);
                            if claim_matching_op(group, flags, |op| {
                                anchor == op.primary_type_symbol.as_deref()
                            }) {
                                ("ok   ", anchor)
                            } else {
                                // Show the first unclaimed op's anchor as the
                                // nearest-actual for the report.
                                let nearest = group
                                    .iter()
                                    .zip(flags.iter())
                                    .find(|(_, claimed)| !**claimed)
                                    .and_then(|(op, _)| op.primary_type_symbol.as_deref());
                                ("MISS ", nearest)
                            }
                        }
                    };
                    eprintln!(
                        "[diag]   anchor[{mark}] {key:?} expected={anchor:?} actual={actual:?}"
                    );
                },
            );
        }
        let compat_cell = if compat_scored {
            format!("{:.2}", cap.compat_verdict)
        } else {
            "UNSCORED".to_string()
        };
        println!(
            "  run {run_idx}/{runs_n}: OVERALL {:.0}%  | endpoint F1 {:.2}  call F1 {:.2}  \
             match F1 {:.2}  anchor {:.2}  resolution {:.2}  compat {compat_cell}  dep F1 {:.2}  \
             decoy_leak {}",
            overall_correctness(cap, compat_scored) * 100.0,
            cap.ep_prf.2,
            cap.call_prf.2,
            cap.match_prf.2,
            cap.type_anchor,
            cap.type_resolution,
            cap.dep_prf.2,
            score.decoy_leak,
        );
        scores.push(score);
    }
    let n = scores.len();
    assert!(n > 0, "[eval] every run failed");

    // §7 (#226): a run where the answer key expected verdicts but NO actual edge
    // carried one is compat-absent. Treat the metric as unscored corpus-wide if
    // *any* run is absent — the aggregate would otherwise mix real verdicts with
    // meaningless 0.0s. The first absence message is kept for the loud report.
    // #431: an UNLABELLED slice (no labelled edge carries a verdict) is also
    // unscored — the vacuous 1.00 must never print as a real score.
    let compat_absent = scores.iter().any(|s| s.compat.is_err());
    let compat_err_msg = scores.iter().find_map(|s| s.compat.as_ref().err().cloned());
    let compat_unscored_reason: Option<&str> = if !compat_labeled {
        Some("no labelled edge carries a compat verdict — abstention, not a pass")
    } else if compat_absent {
        Some("§7: no run produced a non-null verdict")
    } else {
        None
    };

    report_tier(&scores, TIER_CAPABILITY, n, runs_n, compat_unscored_reason);
    report_tier(&scores, TIER_ROADMAP, n, runs_n, compat_unscored_reason);

    let (decoy_mean, decoy_sd) = mean_sd(
        &scores
            .iter()
            .map(|s| s.decoy_leak as f64)
            .collect::<Vec<_>>(),
    );
    println!("\n  decoy_leak (corpus-wide, lower is better)  {decoy_mean:.2}±{decoy_sd:.2}");
    println!(
        "=== end cross-repo live scorer (report-only; the §7 compat-absence guard \
         reds the run at the end) ===\n"
    );

    // The record rides the capability-tier numbers (corpus is all-capability).
    let (ep_p, ep_p_sd) = agg(&scores, TIER_CAPABILITY, |t| t.ep_prf.0);
    let (ep_r, ep_r_sd) = agg(&scores, TIER_CAPABILITY, |t| t.ep_prf.1);
    let (ep_f, ep_f_sd) = agg(&scores, TIER_CAPABILITY, |t| t.ep_prf.2);
    let (cl_p, cl_p_sd) = agg(&scores, TIER_CAPABILITY, |t| t.call_prf.0);
    let (cl_r, cl_r_sd) = agg(&scores, TIER_CAPABILITY, |t| t.call_prf.1);
    let (cl_f, cl_f_sd) = agg(&scores, TIER_CAPABILITY, |t| t.call_prf.2);
    let (m_p, m_p_sd) = agg(&scores, TIER_CAPABILITY, |t| t.match_prf.0);
    let (m_r, m_r_sd) = agg(&scores, TIER_CAPABILITY, |t| t.match_prf.1);
    let (m_f, m_f_sd) = agg(&scores, TIER_CAPABILITY, |t| t.match_prf.2);
    let (own, own_sd) = agg(&scores, TIER_CAPABILITY, |t| t.owner);
    let (anc, anc_sd) = agg(&scores, TIER_CAPABILITY, |t| t.type_anchor);
    let (res, res_sd) = agg(&scores, TIER_CAPABILITY, |t| t.type_resolution);
    // §7 (#226) / #431: the longitudinal record must never store an unscored
    // compat number as a real verdict — absent compat aggregates a meaningless
    // 0.0, an unlabelled slice a vacuous 1.0 — so null out the record's compat
    // columns (the `Option` fields below) in either case.
    let (cmp, cmp_sd) = agg(&scores, TIER_CAPABILITY, |t| t.compat_verdict);
    let (compat_mean_col, compat_sd_col) = if compat_unscored_reason.is_some() {
        (None, None)
    } else {
        (Some(cmp), Some(cmp_sd))
    };
    let (dp_f, dp_f_sd) = agg(&scores, TIER_CAPABILITY, |t| t.dep_prf.2);

    let record = EvalRunRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        ts_unix: now_unix(),
        scanner_version: env!("CARGO_PKG_VERSION").to_string(),
        carrick_sha: std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local".to_string()),
        github_run_id: std::env::var("GITHUB_RUN_ID").ok(),
        fixture: corpus_name(),
        runs_requested: runs_n,
        runs_effective: n,
        model_id: None,
        prompt_hash: None,
        ep_precision_mean: ep_p,
        ep_precision_sd: ep_p_sd,
        ep_recall_mean: ep_r,
        ep_recall_sd: ep_r_sd,
        ep_f1_mean: ep_f,
        ep_f1_sd: ep_f_sd,
        ep_pass_at_k: 0.0,
        ep_pass_pow_k: 0.0,
        call_precision_mean: cl_p,
        call_precision_sd: cl_p_sd,
        call_recall_mean: cl_r,
        call_recall_sd: cl_r_sd,
        call_f1_mean: cl_f,
        call_f1_sd: cl_f_sd,
        call_pass_at_k: 0.0,
        call_pass_pow_k: 0.0,
        note: Some(
            "S4 full vector: endpoint/call/match P/R/F1 + owner/anchor/resolution/compat + \
             deps/orphans/decoys (capability tier)"
                .to_string(),
        ),
        tier: Some("xrepo".to_string()),
        corpus: Some(corpus_name()),
        match_precision_mean: Some(m_p),
        match_precision_sd: Some(m_p_sd),
        match_recall_mean: Some(m_r),
        match_recall_sd: Some(m_r_sd),
        match_f1_mean: Some(m_f),
        match_f1_sd: Some(m_f_sd),
        type_anchor_accuracy_mean: Some(anc),
        type_anchor_accuracy_sd: Some(anc_sd),
        type_resolution_accuracy_mean: Some(res),
        type_resolution_accuracy_sd: Some(res_sd),
        compat_verdict_accuracy_mean: compat_mean_col,
        compat_verdict_accuracy_sd: compat_sd_col,
        owner_accuracy_mean: Some(own),
        owner_accuracy_sd: Some(own_sd),
        decoy_leak_mean: Some(decoy_mean),
        decoy_leak_sd: Some(decoy_sd),
        dep_f1_mean: Some(dp_f),
        dep_f1_sd: Some(dp_f_sd),
    };
    emit_record(&record);

    // §7 compat-absence guard (#226), moved to the VERY END so the full vector +
    // the JSONL record print first. If the answer key expected verdicts but no
    // run produced one, cross-repo type checking was silently skipped — red the
    // run (non-zero exit) so it is still flagged, without hiding owner / anchor /
    // resolution / deps / decoys behind an early panic. The diag dump above
    // disambiguates the cause: type-resolution acc > 0 ⇒ inference WAS reached ⇒
    // the gap is the cross-repo check / verdict step (not the type pipeline).
    if let Some(msg) = compat_err_msg {
        panic!(
            "[eval] §7 compat-absence guard tripped (run flagged RED, but the full \
             metric vector + record above are valid): {msg}"
        );
    }
}

/// MOCK-mode plumbing smoke check (offline, no OIDC, runs under plain
/// `cargo test --test eval_xrepo -- --ignored mock_smoke`). Proves the two-phase
/// harness spans all 3 repos and produces a parseable `EvalProjection` without
/// crashing. Matches may be ~empty under the heuristic mock — that is EXPECTED;
/// this asserts plumbing, not accuracy. `#[ignore]` because it spawns the binary
/// over the corpus (slow) and is not part of the CI Test Suite allowlist.
#[test]
#[ignore = "mock plumbing smoke: spawns the binary over the corpus (slow, offline)"]
fn xrepo_mock_plumbing_smoke() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_carrick"));
    let corpus = corpus_dir();
    assert!(corpus.is_dir(), "corpus dir missing: {}", corpus.display());

    let proj = run_two_phase(&bin, &corpus, true);
    // Plumbing assertion only: the join produced a non-empty projection spanning
    // the corpus. Accuracy (endpoint/match F1) is NOT asserted in mock mode.
    assert!(
        !proj.endpoints.is_empty(),
        "mock two-phase join produced no endpoints across the 3 repos — plumbing broken"
    );
    println!(
        "[eval] mock smoke: {} endpoints, {} cross-repo matches across the joined corpus",
        proj.endpoints.len(),
        proj.cross_repo_matches.len()
    );
}

// ---------------------------------------------------------------------------
// Scoring-math unit tests — these DO run under plain `cargo test` (no #[ignore],
// no subprocess, no LLM). Synthetic projections + the real corpus labels →
// known numbers. They run via the ci.yml `eval_xrepo` step.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scoring_tests {
    use super::*;

    fn http_ep(method: &str, path: &str) -> EvalOp {
        EvalOp {
            key: format!("http|{}|{}", method.to_uppercase(), path),
            protocol: "http".to_string(),
            method: Some(method.to_string()),
            path: Some(path.to_string()),
            handler: None,
            type_state: None,
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
        }
    }

    fn nonhttp_op(protocol: &str, key: &str) -> EvalOp {
        // path = the bare third segment (field/event), as the projection emits.
        let path = key.split('|').nth(2).map(str::to_string);
        EvalOp {
            key: key.to_string(),
            protocol: protocol.to_string(),
            method: None,
            path,
            handler: None,
            type_state: None,
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
        }
    }

    fn cm(p_repo: &str, p_key: &str, c_repo: &str, c_key: &str) -> EvalCrossRepoMatch {
        EvalCrossRepoMatch {
            producer_repo: p_repo.to_string(),
            producer_key: p_key.to_string(),
            consumer_repo: c_repo.to_string(),
            consumer_key: c_key.to_string(),
            type_compatible: None,
        }
    }

    fn cm_compat(
        p_repo: &str,
        p_key: &str,
        c_repo: &str,
        c_key: &str,
        compat: Option<bool>,
    ) -> EvalCrossRepoMatch {
        EvalCrossRepoMatch {
            producer_repo: p_repo.to_string(),
            producer_key: p_key.to_string(),
            consumer_repo: c_repo.to_string(),
            consumer_key: c_key.to_string(),
            type_compatible: compat,
        }
    }

    fn empty_proj() -> EvalProjection {
        EvalProjection {
            endpoints: vec![],
            calls: vec![],
            cross_repo_matches: vec![],
            dependency_conflicts: vec![],
        }
    }

    // --- Shared primitives ---

    #[test]
    fn norm_path_collapses_all_three_param_syntaxes() {
        assert_eq!(norm_path("/orders/:id"), "/orders/:param");
        assert_eq!(norm_path("/orders/{id}"), "/orders/:param");
        assert_eq!(norm_path("/orders/[id]"), "/orders/:param");
        assert_eq!(norm_path("/orders/"), "/orders");
        assert_eq!(norm_path("/"), "/");
    }

    #[test]
    fn norm_key_uppercases_method_and_normalizes_path() {
        assert_eq!(norm_key("http|get|/orders/{id}"), "http|GET|/orders/:param");
        assert_eq!(norm_key("http|GET|/orders/:id"), "http|GET|/orders/:param");
        // GraphQL/socket keys: the third segment has no path params → unchanged
        // apart from the (no-op) uppercasing of the already-cased middle segment.
        assert_eq!(norm_key("graphql|query|order"), "graphql|QUERY|order");
        assert_eq!(
            norm_key("socket|SERVER->CLIENT|payment:settled"),
            "socket|SERVER->CLIENT|payment:settled"
        );
        // Pub/sub keys are 2-segment (`pubsub|<topic>`) → the splitn(3,'|')
        // never matches the 3-tuple HTTP arm, so they fall through unchanged.
        assert_eq!(norm_key("pubsub|order.placed"), "pubsub|order.placed");
        assert_eq!(
            norm_key("pubsub|metrics.page_view"),
            "pubsub|metrics.page_view"
        );
        assert_eq!(norm_key("not-a-key"), "not-a-key");
    }

    /// Ungated parse smoke for whatever corpus is selected (`CARRICK_EVAL_CORPUS`).
    /// Verifies every `<repo>/expected.json` deserializes to `ExpectedRepo` and the
    /// corpus `expected-output.json` to `ExpectedOutput`, with a message naming the
    /// offending file on failure. Runs for ANY corpus (not gated on the default),
    /// so it offline-validates corpus-2's additive `pubsub_operations` arrays.
    #[test]
    fn corpus_expected_files_parse() {
        let corpus = corpus_dir();
        // The default corpus dir always exists, so a missing dir here means a
        // genuinely-typo'd `CARRICK_EVAL_CORPUS`. Skipping would let that typo
        // pass silently, so fail loudly instead.
        assert!(
            corpus.is_dir(),
            "corpus dir {} not found — CARRICK_EVAL_CORPUS may be mistyped",
            corpus.display()
        );
        let repos = discover_repos(&corpus);
        for repo in &repos {
            let path = repo.join("expected.json");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let parsed: Result<ExpectedRepo, _> = serde_json::from_str(&text);
            assert!(
                parsed.is_ok(),
                "parse {} into ExpectedRepo failed: {}",
                path.display(),
                parsed.unwrap_err()
            );
        }

        let out_path = corpus.join("expected-output.json");
        let out_text = std::fs::read_to_string(&out_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", out_path.display()));
        let out_parsed: Result<ExpectedOutput, _> = serde_json::from_str(&out_text);
        assert!(
            out_parsed.is_ok(),
            "parse {} into ExpectedOutput failed: {}",
            out_path.display(),
            out_parsed.unwrap_err()
        );
    }

    #[test]
    fn collapse_ws_normalizes_whitespace() {
        assert_eq!(
            collapse_ws("{ id:  number;\n  x: string }"),
            "{ id: number; x: string }"
        );
        assert_eq!(collapse_ws("  a  b  "), "a b");
    }

    #[test]
    fn prf_contract_conventions() {
        assert_eq!(prf(0, 0, 0), (1.0, 1.0, 1.0));
        assert_eq!(prf(0, 2, 0).0, 0.0);
        assert_eq!(prf(3, 3, 3), (1.0, 1.0, 1.0));
        let (p, r, _) = prf(1, 1, 2);
        assert_eq!((p, r), (1.0, 0.5));
    }

    fn full_marks_tier() -> TierScore {
        TierScore {
            ep_prf: (1.0, 1.0, 1.0),
            call_prf: (1.0, 1.0, 1.0),
            match_prf: (1.0, 1.0, 1.0),
            owner: 1.0,
            type_anchor: 1.0,
            type_resolution: 1.0,
            compat_verdict: 1.0,
            dep_prf: (1.0, 1.0, 1.0),
            orphan_prf: (1.0, 1.0, 1.0),
        }
    }

    #[test]
    fn run_needs_diag_silent_on_full_marks() {
        assert!(!run_needs_diag(&full_marks_tier(), false, true, 0));
    }

    #[test]
    fn run_needs_diag_fires_on_any_dimension_dip() {
        let mut cap = full_marks_tier();
        cap.type_resolution = 0.93;
        assert!(run_needs_diag(&cap, false, true, 0));

        let mut cap = full_marks_tier();
        cap.compat_verdict = 0.5;
        assert!(run_needs_diag(&cap, false, true, 0));

        let mut cap = full_marks_tier();
        cap.match_prf.2 = 0.99;
        assert!(run_needs_diag(&cap, false, true, 0));
    }

    #[test]
    fn run_needs_diag_fires_on_decoy_leak_and_absent_compat() {
        assert!(run_needs_diag(&full_marks_tier(), false, true, 1));
        // §7 compat-absent (guard failed): the compat_verdict number is
        // meaningless, so the absence itself is the dip signal regardless of
        // the other dimensions.
        assert!(run_needs_diag(&full_marks_tier(), true, false, 0));
    }

    /// #431: an UNLABELLED compat slice (guard clean, nothing to score) is a
    /// property of the labels, not a dip — full marks stay silent.
    #[test]
    fn run_needs_diag_silent_on_unlabelled_compat() {
        assert!(!run_needs_diag(&full_marks_tier(), false, false, 0));
    }

    #[test]
    fn mean_sd_sample_variance() {
        let (m, sd) = mean_sd(&[1.0, 1.0, 1.0]);
        assert_eq!(m, 1.0);
        assert_eq!(sd, 0.0);
        let (m, sd) = mean_sd(&[0.0, 1.0]);
        assert_eq!(m, 0.5);
        assert!((sd - 0.5_f64.sqrt()).abs() < 1e-9);
        assert_eq!(mean_sd(&[]), (0.0, 0.0));
    }

    // --- Synthetic-projection metric tests ---

    #[test]
    fn nonhttp_op_key_keeps_kind_and_direction() {
        // The bare path drops kind/direction; the canonical key keeps it, so the
        // op-set key must derive from `key`, not `(method,path)`.
        let q = nonhttp_op("graphql", "graphql|query|order");
        let s = nonhttp_op("graphql", "graphql|subscription|order"); // same field, diff kind
        assert_ne!(op_to_set_key(&q), op_to_set_key(&s));
        let listen = nonhttp_op("socket", "socket|SERVER->CLIENT|payment:settled");
        let emit = nonhttp_op("socket", "socket|CLIENT->SERVER|payment:settled");
        assert_ne!(op_to_set_key(&listen), op_to_set_key(&emit));
    }

    #[test]
    fn match_set_exact_with_normalization() {
        let expected = vec![
            ExpMatch {
                producer_repo: "orders-pkg".to_string(),
                producer_key: "http|GET|/orders/:param".to_string(),
                consumer_repo: "payments-svc".to_string(),
                consumer_key: "http|GET|/orders/:param".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
            ExpMatch {
                producer_repo: "payments-svc".to_string(),
                producer_key: "http|POST|/payments".to_string(),
                consumer_repo: "web-frontend".to_string(),
                consumer_key: "http|POST|/payments".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
        ];
        // The found edge uses {id}/[id] + lowercased method → normalizes to match
        // the first expected edge; the second is missing.
        let found = vec![cm(
            "orders-pkg",
            "http|get|/orders/{id}",
            "payments-svc",
            "http|GET|/orders/[id]",
        )];
        let (p, r, _f) = score_matches(&expected, &found, TIER_CAPABILITY);
        assert_eq!(p, 1.0, "the one found edge matched after normalization");
        assert_eq!(r, 0.5, "one of two expected edges found");
    }

    #[test]
    fn match_set_spans_graphql_and_socket() {
        let expected = vec![
            ExpMatch {
                producer_repo: "gateway".to_string(),
                producer_key: "graphql|query|order".to_string(),
                consumer_repo: "web-frontend".to_string(),
                consumer_key: "graphql|query|order".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
            ExpMatch {
                producer_repo: "web-frontend".to_string(),
                producer_key: "socket|SERVER->CLIENT|payment:settled".to_string(),
                consumer_repo: "payments-svc".to_string(),
                consumer_key: "socket|SERVER->CLIENT|payment:settled".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
        ];
        let found = vec![
            cm(
                "gateway",
                "graphql|query|order",
                "web-frontend",
                "graphql|query|order",
            ),
            cm(
                "web-frontend",
                "socket|SERVER->CLIENT|payment:settled",
                "payments-svc",
                "socket|SERVER->CLIENT|payment:settled",
            ),
        ];
        let (p, r, f) = score_matches(&expected, &found, TIER_CAPABILITY);
        assert_eq!((p, r, f), (1.0, 1.0, 1.0), "both non-HTTP edges matched");
    }

    #[test]
    fn decoy_leak_counts_must_not_emit_hits() {
        let repos = vec![(
            "payments-svc".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "_must_not_emit": [
                    { "kind": "call", "method": "*", "path": "/__sdk__/PutItemCommand" },
                    { "kind": "endpoint", "method": "POST", "path": "/__lambda__/settle" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        proj.calls
            .push(http_ep("DELETE", "/__sdk__/PutItemCommand")); // leak (method *)
        proj.endpoints.push(http_ep("POST", "/__lambda__/settle")); // leak
        proj.endpoints.push(http_ep("GET", "/__lambda__/settle")); // NOT a leak (wrong method)
        proj.endpoints.push(http_ep("GET", "/orders/1")); // clean
        assert_eq!(score_decoy_leak(&repos, &proj), 2);
    }

    #[test]
    fn owner_accuracy_over_intersection() {
        let repos = vec![(
            "orders-pkg".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "endpoints": [
                    { "method": "GET", "path": "/orders/:id", "owner": "ordersRouter", "tier": "capability" },
                    { "method": "GET", "path": "/gateway/health", "owner": "healthCheckHandler", "tier": "capability" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        let mut ok = http_ep("GET", "/orders/:id");
        ok.handler = Some("ordersRouter".to_string());
        let mut fab = http_ep("GET", "/gateway/health");
        fab.handler = Some("GET".to_string()); // owner=method fabrication
        proj.endpoints.push(ok);
        proj.endpoints.push(fab);
        // 2 in both sets, 1 correct.
        assert!((score_owner_accuracy(&repos, &proj, TIER_CAPABILITY) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn type_anchor_accuracy_null_equals_null() {
        let repos = vec![(
            "gateway".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "endpoints": [
                    { "method": "GET", "path": "/users/:id", "primary_type_symbol": "UserSummary", "tier": "capability" },
                    { "method": "GET", "path": "/users/recent", "primary_type_symbol": null, "tier": "capability" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        let mut a = http_ep("GET", "/users/:id");
        a.primary_type_symbol = Some("UserSummary".to_string());
        let b = http_ep("GET", "/users/recent"); // anchor None == expected null
        proj.endpoints.push(a);
        proj.endpoints.push(b);
        assert!((score_type_anchor_accuracy(&repos, &proj, TIER_CAPABILITY) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn type_resolution_state_and_whitespace() {
        let repos = vec![(
            "orders-pkg".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "endpoints": [
                    { "method": "GET", "path": "/orders/:id",
                      "resolved_type": "{ id: number; amountCents: number; }",
                      "type_state": "Explicit", "tier": "capability" },
                    { "method": "GET", "path": "/users/recent",
                      "resolved_type": "{ count: number; ids: string[]; }",
                      "type_state": "Implicit", "tier": "capability" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        let mut a = http_ep("GET", "/orders/:id");
        // Same type, different spacing → whitespace-collapsed eq.
        a.resolved_definition = Some("{ id: number;   amountCents: number; }".to_string());
        a.type_state = Some("Explicit".to_string());
        let mut b = http_ep("GET", "/users/recent");
        b.resolved_definition = Some("{ count: number; ids: string[]; }".to_string());
        b.type_state = Some("Explicit".to_string()); // WRONG state (should be Implicit)
        proj.endpoints.push(a);
        proj.endpoints.push(b);
        // a correct, b wrong (state mismatch) → 0.5.
        assert!(
            (score_type_resolution_accuracy(&repos, &proj, TIER_CAPABILITY) - 0.5).abs() < 1e-9
        );
    }

    /// carrick#735: the sidecar prints a union's members in a canonical order,
    /// which is not the order the corpus labels were authored in. The label is
    /// the answer key and is not edited to follow a printer change, so the
    /// comparison ignores member order — and only member order. A different
    /// member SET is still a miss.
    #[test]
    fn type_resolution_ignores_union_member_order() {
        let repos = vec![(
            "support-desk".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "endpoints": [
                    { "method": "GET", "path": "/tickets/:id",
                      "resolved_type": "{ id: string; status: \"OPEN\" | \"ESCALATED\" | \"CLOSED\"; }",
                      "type_state": "Explicit", "tier": "capability" },
                    { "method": "GET", "path": "/tickets",
                      "resolved_type": "{ id: string; status: \"OPEN\" | \"ESCALATED\" | \"CLOSED\"; }",
                      "type_state": "Explicit", "tier": "capability" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        let mut reordered = http_ep("GET", "/tickets/:id");
        // Same members, canonical order → a hit.
        reordered.expanded_definition =
            Some("{ id: string; status: \"CLOSED\" | \"ESCALATED\" | \"OPEN\"; }".to_string());
        reordered.type_state = Some("Explicit".to_string());
        let mut wrong_set = http_ep("GET", "/tickets");
        // A member the label does not carry → still a miss, whatever the order.
        wrong_set.expanded_definition =
            Some("{ id: string; status: \"CLOSED\" | \"ESCALATED\" | \"PENDING\"; }".to_string());
        wrong_set.type_state = Some("Explicit".to_string());
        proj.endpoints.push(reordered);
        proj.endpoints.push(wrong_set);
        assert!(
            (score_type_resolution_accuracy(&repos, &proj, TIER_CAPABILITY) - 0.5).abs() < 1e-9
        );
    }

    /// The union-order normalisation must not quietly equate different types.
    #[test]
    fn canonical_type_text_ignores_order_only() {
        // Order only, at the top level and nested inside an object member.
        assert_eq!(
            canonical_type_text("\"OPEN\" | \"ESCALATED\" | \"CLOSED\""),
            canonical_type_text("\"CLOSED\" | \"ESCALATED\" | \"OPEN\"")
        );
        assert_eq!(
            canonical_type_text("{ id: string; status: \"a\" | \"b\"; }"),
            canonical_type_text("{ id: string; status: \"b\" | \"a\" }")
        );
        assert_eq!(
            canonical_type_text("(\"a\" | \"b\")[]"),
            canonical_type_text("(\"b\" | \"a\")[]")
        );
        assert_eq!(
            canonical_type_text("Promise<string | null>"),
            canonical_type_text("Promise<null | string>")
        );

        // Everything that is not member order still separates.
        assert_ne!(
            canonical_type_text("\"a\" | \"b\""),
            canonical_type_text("\"a\" | \"c\"")
        );
        assert_ne!(
            canonical_type_text("\"a\" | \"b\""),
            canonical_type_text("\"a\" | \"b\" | \"c\"")
        );
        assert_ne!(
            canonical_type_text("{ a: string; b: number; }"),
            canonical_type_text("{ b: number; a: string; }"),
        );
        assert_ne!(
            canonical_type_text("{ a?: string; }"),
            canonical_type_text("{ a: string; }")
        );
        assert_ne!(
            canonical_type_text("string[]"),
            canonical_type_text("string")
        );
        // A `|` inside a string literal is not a member separator.
        assert_eq!(
            canonical_type_text("\"a|b\" | \"c\""),
            canonical_type_text("\"c\" | \"a|b\"")
        );
        assert_ne!(
            canonical_type_text("\"a|b\""),
            canonical_type_text("\"a\" | \"b\"")
        );
        // A function type's arrow is left alone rather than mis-parsed.
        assert_eq!(
            canonical_type_text("{ cb: (x: any) => void; }"),
            canonical_type_text("{ cb: (x: any) => void }")
        );
    }

    /// Fan-in (#290/#291): two expected consumer slots on ONE key (two repos
    /// publishing the same pub/sub topic with different payloads) must each
    /// claim a DISTINCT actual op. The old first-op join scored both slots
    /// against whichever op the index kept — the flat-payload slot passed and
    /// the nested-payload slot could never match.
    #[test]
    fn type_resolution_fan_in_slots_claim_distinct_ops() {
        let repos = vec![
            (
                "billing-svc".to_string(),
                serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                    "pubsub_operations": [
                        { "key": "pubsub|order.placed", "role": "consumer",
                          "resolved_type": "{ id: string; total: number; }",
                          "type_state": "Explicit", "tier": "capability" }
                    ]
                }))
                .unwrap(),
            ),
            (
                "orders-engine".to_string(),
                serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                    "pubsub_operations": [
                        { "key": "pubsub|order.placed", "role": "consumer",
                          "resolved_type": "{ id: string; total: { amountCents: number; }; }",
                          "type_state": "Explicit", "tier": "capability" }
                    ]
                }))
                .unwrap(),
            ),
        ];
        let mut proj = empty_proj();
        let mut flat = nonhttp_op("pubsub", "pubsub|order.placed");
        flat.resolved_definition = Some("{ id: string; total: number; }".to_string());
        flat.type_state = Some("Explicit".to_string());
        let mut nested = nonhttp_op("pubsub", "pubsub|order.placed");
        nested.resolved_definition =
            Some("{ id: string; total: { amountCents: number; }; }".to_string());
        nested.type_state = Some("Explicit".to_string());
        proj.calls.push(flat);
        proj.calls.push(nested);
        // Both slots find their own op → 1.0 (was 0.5 under the first-op join).
        assert!(
            (score_type_resolution_accuracy(&repos, &proj, TIER_CAPABILITY) - 1.0).abs() < 1e-9
        );
    }

    /// The claiming join is 1:1: when the projection carries only ONE op for a
    /// fan-in key (the other site's op is genuinely missing), a single actual
    /// op must not double-count against both expected slots.
    #[test]
    fn type_resolution_single_op_cannot_satisfy_two_slots() {
        let repos = vec![
            (
                "billing-svc".to_string(),
                serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                    "pubsub_operations": [
                        { "key": "pubsub|order.placed", "role": "consumer",
                          "resolved_type": "{ id: string; total: number; }",
                          "type_state": "Explicit", "tier": "capability" }
                    ]
                }))
                .unwrap(),
            ),
            (
                "orders-engine".to_string(),
                serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                    "pubsub_operations": [
                        { "key": "pubsub|order.placed", "role": "consumer",
                          "resolved_type": "{ id: string; total: number; }",
                          "type_state": "Explicit", "tier": "capability" }
                    ]
                }))
                .unwrap(),
            ),
        ];
        let mut proj = empty_proj();
        let mut only = nonhttp_op("pubsub", "pubsub|order.placed");
        only.resolved_definition = Some("{ id: string; total: number; }".to_string());
        only.type_state = Some("Explicit".to_string());
        proj.calls.push(only);
        // Two slots joined (key present), one distinct op to claim → 0.5.
        assert!(
            (score_type_resolution_accuracy(&repos, &proj, TIER_CAPABILITY) - 0.5).abs() < 1e-9
        );
    }

    #[test]
    fn compat_verdict_accuracy_and_none_is_miss() {
        let expected = vec![
            ExpMatch {
                producer_repo: "orders-pkg".to_string(),
                producer_key: "http|GET|/orders/:param".to_string(),
                consumer_repo: "payments-svc".to_string(),
                consumer_key: "http|GET|/orders/:param".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
            ExpMatch {
                producer_repo: "orders-pkg".to_string(),
                producer_key: "http|GET|/orders/:param".to_string(),
                consumer_repo: "web-frontend".to_string(),
                consumer_key: "http|GET|/orders/:param".to_string(),
                type_compatible: Some(false),
                tier: TIER_CAPABILITY.to_string(),
            },
        ];
        let found = vec![
            cm_compat(
                "orders-pkg",
                "http|GET|/orders/:param",
                "payments-svc",
                "http|GET|/orders/:param",
                Some(true), // correct
            ),
            cm_compat(
                "orders-pkg",
                "http|GET|/orders/:param",
                "web-frontend",
                "http|GET|/orders/:param",
                None, // actual None on a labelled edge → miss
            ),
        ];
        // 2 edges in both sets, 1 verdict correct.
        assert!(
            (score_compat_verdict_accuracy(&expected, &found, TIER_CAPABILITY) - 0.5).abs() < 1e-9
        );
    }

    #[test]
    fn compat_guard_fails_when_verdict_absent() {
        let expected: ExpectedOutput = serde_json::from_value(serde_json::json!({
            "matches": [
                { "producer_repo": "a", "producer_key": "http|GET|/x",
                  "consumer_repo": "b", "consumer_key": "http|GET|/x",
                  "type_compatible": true, "tier": "capability" }
            ]
        }))
        .unwrap();
        // No actual edge carries a verdict → guard trips.
        let absent = vec![cm("a", "http|GET|/x", "b", "http|GET|/x")];
        assert!(compat_guard(&expected.matches, &absent).is_err());
        // At least one verdict present → guard passes.
        let present = vec![cm_compat(
            "a",
            "http|GET|/x",
            "b",
            "http|GET|/x",
            Some(true),
        )];
        assert!(compat_guard(&expected.matches, &present).is_ok());
        // No edge expects a verdict → guard vacuously passes.
        let no_expect: ExpectedOutput = serde_json::from_value(serde_json::json!({
            "matches": [
                { "producer_repo": "a", "producer_key": "http|GET|/x",
                  "consumer_repo": "b", "consumer_key": "http|GET|/x", "tier": "capability" }
            ]
        }))
        .unwrap();
        assert!(compat_guard(&no_expect.matches, &absent).is_ok());
    }

    /// #226 regression: when compat is silently absent, `score_corpus` must NOT
    /// panic — it returns the FULL vector with `compat: Err(..)` so owner /
    /// anchor / resolution / deps / decoys still print. The compat-verdict number
    /// is 0.0 (0/0) but is marked UNSCORED by the caller, never read as a
    /// "0% compatible" verdict.
    #[test]
    fn score_corpus_returns_full_vector_when_compat_absent() {
        let corpus = corpus_dir();
        if !corpus.is_dir() {
            eprintln!("[eval] corpus dir missing — skipping");
            return;
        }
        let repos = discover_repos(&corpus);
        let repo_expected = load_repo_expected(&repos);
        let expected_output = load_expected_output(&corpus);

        // A projection that DOES match the corpus topology (so the non-compat
        // metrics are meaningful) but whose matches carry NO verdict → compat
        // absent. We reuse the perfect-projection builder's matches sans verdict
        // by emitting one real edge with `type_compatible = None`.
        let mut proj = empty_proj();
        // One real labelled edge, but with the verdict stripped (None).
        if let Some(first) = expected_output.matches.first() {
            proj.cross_repo_matches.push(cm(
                &first.producer_repo,
                &first.producer_key,
                &first.consumer_repo,
                &first.consumer_key,
            ));
        }

        let score = score_corpus(
            &proj,
            &repo_expected,
            &expected_output,
            &load_repo_aliases(&repos),
        );
        assert!(
            score.compat.is_err(),
            "the corpus expects verdicts + no actual edge carries one → §7 trips"
        );
        // The full vector is still present (not panicked away).
        let cap = score
            .by_tier
            .get(TIER_CAPABILITY)
            .expect("capability tier is still scored despite absent compat");
        // compat_verdict is the meaningless 0/0 → 0.0; the caller renders it
        // UNSCORED rather than reading it as a real verdict.
        assert_eq!(
            cap.compat_verdict, 0.0,
            "absent compat scores 0/0 → 0.0 (rendered UNSCORED by the caller)"
        );
        // The roadmap tier is also present — both tiers were scored.
        assert!(score.by_tier.contains_key(TIER_ROADMAP));
    }

    #[test]
    fn dep_conflicts_exact_set() {
        let expected = vec![ExpDepConflict {
            package: "zod".to_string(),
            versions: vec!["3.22.0".to_string(), "4.0.0".to_string()],
            severity: "critical".to_string(),
            tier: TIER_CAPABILITY.to_string(),
        }];
        // Found out-of-order versions normalize to a match.
        let found = vec![EvalDependencyConflict {
            package: "zod".to_string(),
            versions: vec!["4.0.0".to_string(), "3.22.0".to_string()],
            severity: "critical".to_string(),
        }];
        assert_eq!(
            score_dep_conflicts(&expected, &found, TIER_CAPABILITY),
            (1.0, 1.0, 1.0)
        );
        // Wrong severity → no match.
        let wrong = vec![EvalDependencyConflict {
            package: "zod".to_string(),
            versions: vec!["3.22.0".to_string(), "4.0.0".to_string()],
            severity: "warning".to_string(),
        }];
        let (p, r, _) = score_dep_conflicts(&expected, &wrong, TIER_CAPABILITY);
        assert_eq!((p, r), (0.0, 0.0));
    }

    #[test]
    fn orphans_recall_over_matched_complement() {
        let expected = vec![
            ExpOrphan {
                repo: "orders-pkg".to_string(),
                side: "producer".to_string(),
                key: "http|GET|/api/v1/status".to_string(),
                tier: TIER_CAPABILITY.to_string(),
            },
            ExpOrphan {
                repo: "payments-svc".to_string(),
                side: "consumer".to_string(),
                key: "http|POST|/billing/charge".to_string(),
                tier: TIER_CAPABILITY.to_string(),
            },
        ];
        // The matcher matched /billing/charge (so it is NOT actually an orphan →
        // recall miss); /api/v1/status stays unmatched → correct orphan.
        let matches = vec![cm(
            "orders-pkg",
            "http|POST|/billing",
            "payments-svc",
            "http|POST|/billing/charge",
        )];
        let (_p, r, _f) = score_orphans(&expected, &matches, TIER_CAPABILITY);
        assert!(
            (r - 0.5).abs() < 1e-9,
            "1 of 2 labelled orphans confirmed unmatched"
        );
    }

    #[test]
    fn roadmap_tier_empty_scores_perfect() {
        // No roadmap labels anywhere → every per-tier metric is the §1.4
        // expected-0 perfect score (nothing expected, nothing scored).
        let repos: Vec<(String, ExpectedRepo)> = vec![];
        let proj = empty_proj();
        let out: ExpectedOutput = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            score_endpoint_set(&repos, &proj, TIER_ROADMAP),
            (1.0, 1.0, 1.0)
        );
        assert_eq!(score_call_set(&repos, &proj, TIER_ROADMAP), (1.0, 1.0, 1.0));
        assert_eq!(score_owner_accuracy(&repos, &proj, TIER_ROADMAP), 1.0);
        assert_eq!(
            score_dep_conflicts(
                &out.dependency_conflicts,
                &proj.dependency_conflicts,
                TIER_ROADMAP
            ),
            (1.0, 1.0, 1.0)
        );
    }

    // --- Real-corpus-label tests (deterministic; load the committed labels) ---

    #[test]
    fn perfect_synthetic_projection_against_real_corpus() {
        // Pins corpus-1's exact 3-repo structure; skip for any other corpus.
        if corpus_name() != "xrepo-corpus-1" {
            return;
        }
        let corpus = corpus_dir();
        let repos = discover_repos(&corpus);
        assert_eq!(repos.len(), 3, "the 3 top-level corpus repos");
        let repo_expected = load_repo_expected(&repos);
        let expected_output = load_expected_output(&corpus);

        // Build a projection whose endpoint/call sets equal the union of all
        // repos' expected ops, whose matches equal the expected edges (with their
        // labelled compat verdict), and whose deps equal the labelled conflict.
        let mut endpoints: Vec<EvalOp> = Vec::new();
        let mut calls: Vec<EvalOp> = Vec::new();
        for (_repo, exp) in &repo_expected {
            for e in &exp.endpoints {
                let mut op = http_ep(&e.method, &e.path);
                op.handler = e.owner.clone();
                op.primary_type_symbol = e.primary_type_symbol.clone();
                op.resolved_definition = e.resolved_type.clone();
                op.type_state = e.type_state.clone();
                endpoints.push(op);
            }
            for c in &exp.calls {
                // Mirror the consumer call so the fuzzy match fires: key carries
                // the host token, path carries the path token.
                let host = c.host_contains.clone().unwrap_or_default();
                let path = c.path.clone();
                let mut op = http_ep(&c.method, &path);
                op.key = format!("http|{}|{}/{}", c.method.to_uppercase(), host, path);
                op.path = Some(path);
                calls.push(op);
            }
            for op in exp
                .graphql_operations
                .iter()
                .chain(exp.socket_events.iter())
                .chain(exp.pubsub_operations.iter())
            {
                let proto = op.key.split('|').next().unwrap_or("");
                let mut eo = nonhttp_op(proto, &op.key);
                eo.primary_type_symbol = op.primary_type_symbol.clone();
                eo.resolved_definition = op.resolved_type.clone();
                eo.type_state = op.type_state.clone();
                if op.role == "producer" {
                    endpoints.push(eo);
                } else {
                    calls.push(eo);
                }
            }
        }
        let cross_repo_matches: Vec<EvalCrossRepoMatch> = expected_output
            .matches
            .iter()
            .map(|m| {
                cm_compat(
                    &m.producer_repo,
                    &m.producer_key,
                    &m.consumer_repo,
                    &m.consumer_key,
                    m.type_compatible,
                )
            })
            .collect();
        let dependency_conflicts: Vec<EvalDependencyConflict> = expected_output
            .dependency_conflicts
            .iter()
            .map(|d| EvalDependencyConflict {
                package: d.package.clone(),
                versions: d.versions.clone(),
                severity: d.severity.clone(),
            })
            .collect();
        let proj = EvalProjection {
            endpoints,
            calls,
            cross_repo_matches,
            dependency_conflicts,
        };

        let score = score_corpus(
            &proj,
            &repo_expected,
            &expected_output,
            &load_repo_aliases(&repos),
        );
        assert!(
            score.compat.is_ok(),
            "§7 guard passes — the perfect projection carries verdicts"
        );
        assert!(
            score.match_zero_join.is_none(),
            "a perfectly-joining projection must not trip the #431 zero-join diagnosis"
        );
        let cap = score.by_tier.get(TIER_CAPABILITY).unwrap();
        assert_eq!(cap.ep_prf, (1.0, 1.0, 1.0), "perfect endpoint set");
        assert_eq!(cap.call_prf.1, 1.0, "perfect call recall");
        assert_eq!(cap.match_prf, (1.0, 1.0, 1.0), "perfect match set");
        assert!((cap.owner - 1.0).abs() < 1e-9, "perfect owner");
        assert!((cap.type_anchor - 1.0).abs() < 1e-9, "perfect anchor");
        assert!(
            (cap.type_resolution - 1.0).abs() < 1e-9,
            "perfect resolution"
        );
        assert!((cap.compat_verdict - 1.0).abs() < 1e-9, "perfect compat");
        assert_eq!(cap.dep_prf, (1.0, 1.0, 1.0), "perfect deps");
        assert!(
            (cap.orphan_prf.1 - 1.0).abs() < 1e-9,
            "perfect orphan recall"
        );
        assert_eq!(score.decoy_leak, 0, "perfect projection emits no decoys");
        // roadmap tier is empty everywhere → perfect by the expected-0 convention.
        let road = score.by_tier.get(TIER_ROADMAP).unwrap();
        assert_eq!(road.ep_prf, (1.0, 1.0, 1.0));
        assert_eq!(road.match_prf, (1.0, 1.0, 1.0));
    }

    #[test]
    fn corpus_has_the_expected_protocol_and_edge_shape() {
        // Pins the corpus invariants the scorer relies on, so a label edit that
        // drifts the shape fails here (answer-key-drift discipline). These edge
        // counts are corpus-1 specific; skip for any other corpus.
        if corpus_name() != "xrepo-corpus-1" {
            return;
        }
        let corpus = corpus_dir();
        let repos = discover_repos(&corpus);
        let repo_expected = load_repo_expected(&repos);
        let expected_output = load_expected_output(&corpus);

        // 8 matched edges: 5 http + 2 graphql + 1 socket. The two extra HTTP
        // edges are the request-body direction fixtures (POST /widgets widening,
        // POST /invoices narrowing) that pin the confirmed request-direction bug.
        assert_eq!(expected_output.matches.len(), 8);
        let n_proto = |p: &str| {
            expected_output
                .matches
                .iter()
                .filter(|m| m.producer_key.starts_with(&format!("{p}|")))
                .count()
        };
        assert_eq!(n_proto("http"), 5, "5 HTTP edges");
        assert_eq!(n_proto("graphql"), 2, "2 GraphQL edges");
        assert_eq!(n_proto("socket"), 1, "1 socket edge");

        // 3 incompatible edges: two REST (orders id string-vs-number, and the
        // POST /invoices request-body narrowing) + one GraphQL subscription.
        let incompatible = expected_output
            .matches
            .iter()
            .filter(|m| m.type_compatible == Some(false))
            .count();
        assert_eq!(incompatible, 3, "3 deliberately-incompatible edges");

        // Every edge carries a non-null verdict → the §7 guard is live.
        assert!(
            expected_output
                .matches
                .iter()
                .all(|m| m.type_compatible.is_some()),
            "every corpus edge labels a compat verdict"
        );

        // The corpus authors the first Implicit type_state (gateway /users/recent).
        let any_implicit = repo_expected.iter().any(|(_r, e)| {
            e.endpoints
                .iter()
                .any(|ep| ep.type_state.as_deref() == Some("Implicit"))
        });
        assert!(
            any_implicit,
            "corpus has its first Implicit type_state case"
        );

        // Decoys are present and total 4 (2 MCP + audit SDK + lambda settle).
        let n_decoys: usize = repo_expected
            .iter()
            .map(|(_r, e)| e.must_not_emit.len())
            .sum();
        assert_eq!(n_decoys, 4, "4 _must_not_emit decoys across the corpus");

        // Dependency conflict: one cross-repo zod 3.x vs 4.x, critical.
        assert_eq!(expected_output.dependency_conflicts.len(), 1);
        let dc = &expected_output.dependency_conflicts[0];
        assert_eq!(dc.package, "zod");
        assert_eq!(dc.severity, "critical");
    }

    /// PROOF (owner ruling 2026-07-12): the undeclared-env-var audit-webhook call
    /// joins the LIVE projected key. payments-svc/lib/audit.ts posts to
    /// `${process.env.AUDIT_WEBHOOK_URL ?? ...}/audit/events`, and because
    /// AUDIT_WEBHOOK_URL is deliberately NOT declared in payments-svc/carrick.json,
    /// the live projection keys the call with the raw env-var base intact:
    /// `http|POST|${AUDIT_WEBHOOK_URL}/audit/events` (post-#294/#354). The offline
    /// mock can't synthesize this LLM-extracted call, so this test loads the
    /// COMMITTED label and asserts it fuzzy-joins that exact op — i.e. the call
    /// scores as an expected consumer, never as an extra.
    #[test]
    fn audit_webhook_label_joins_live_projected_key() {
        if corpus_name() != "xrepo-corpus-1" {
            return;
        }
        let corpus = corpus_dir();
        let repos = discover_repos(&corpus);
        let repo_expected = load_repo_expected(&repos);
        let (_r, payments) = repo_expected
            .iter()
            .find(|(r, _)| r == "payments-svc")
            .expect("payments-svc repo present");
        let audit = payments
            .calls
            .iter()
            .find(|c| c.path_contains.as_deref() == Some("/audit/events"))
            .expect("committed audit-webhook call label present");

        // The op EXACTLY as the live projection emits it: `http_ep` yields
        // key `http|POST|${AUDIT_WEBHOOK_URL}/audit/events` and the same path,
        // the raw-env-var-base form that survives for the undeclared var.
        let live = http_ep("POST", "${AUDIT_WEBHOOK_URL}/audit/events");
        assert_eq!(live.key, "http|POST|${AUDIT_WEBHOOK_URL}/audit/events");
        assert!(
            fuzzy_call_match(audit, &live),
            "committed audit label must join the live env-var-base key"
        );

        // Negative control: same method, different path must NOT join, so the
        // `path_contains` constraint is load-bearing (not match-anything).
        let other = http_ep("POST", "${AUDIT_WEBHOOK_URL}/other/path");
        assert!(
            !fuzzy_call_match(audit, &other),
            "the label's path_contains constraint is load-bearing"
        );
        // Method discrimination: a GET to the same path must NOT join.
        let wrong_method = http_ep("GET", "${AUDIT_WEBHOOK_URL}/audit/events");
        assert!(
            !fuzzy_call_match(audit, &wrong_method),
            "method must match for the fuzzy call join"
        );
    }

    #[test]
    fn decoy_leak_respects_projection_set_kind() {
        // Corpus-3's intra-repo self-call decoy shares (method, path) with the
        // repo's legit endpoint. The `kind: "call"` label must flag only a
        // call-set emission — never the endpoint-set one.
        let repos = vec![(
            "inventory-svc".to_string(),
            serde_json::from_value::<ExpectedRepo>(serde_json::json!({
                "_must_not_emit": [
                    { "kind": "call", "method": "GET", "path": "/warehouses/:warehouseId/stock/:sku" }
                ]
            }))
            .unwrap(),
        )];
        let mut proj = empty_proj();
        // The legit producer endpoint on the same (method, path): clean.
        proj.endpoints
            .push(http_ep("GET", "/warehouses/:warehouseId/stock/:sku"));
        assert_eq!(score_decoy_leak(&repos, &proj), 0);
        // The self-call emitted into the call set: a leak.
        proj.calls
            .push(http_ep("GET", "/warehouses/:wid/stock/:sku"));
        assert_eq!(score_decoy_leak(&repos, &proj), 1);
    }

    #[test]
    fn corpus3_has_the_expected_protocol_and_edge_shape() {
        // Answer-key-drift guard for xrepo-corpus-3; skip for other corpora.
        if corpus_name() != "xrepo-corpus-3" {
            return;
        }
        let corpus = corpus_dir();
        let repos = discover_repos(&corpus);
        assert_eq!(repos.len(), 7, "the 7 top-level corpus repos");
        let repo_expected = load_repo_expected(&repos);
        let expected_output = load_expected_output(&corpus);

        // 12 matched edges: 4 http + 5 pubsub + 2 graphql + 1 socket, all capability.
        assert_eq!(expected_output.matches.len(), 12);
        let n_proto = |p: &str| {
            expected_output
                .matches
                .iter()
                .filter(|m| m.producer_key.starts_with(&format!("{p}|")))
                .count()
        };
        assert_eq!(n_proto("http"), 4, "4 HTTP edges");
        assert_eq!(n_proto("pubsub"), 5, "5 pub/sub edges");
        assert_eq!(n_proto("graphql"), 2, "2 GraphQL edges");
        assert_eq!(n_proto("socket"), 1, "1 socket edge");
        assert!(
            expected_output
                .matches
                .iter()
                .all(|m| m.tier == TIER_CAPABILITY),
            "every corpus-3 edge is capability tier"
        );

        // 4 incompatible edges (null-vs-optional, array-vs-scalar, Date-vs-string,
        // missing-required-field) and a live §7 guard on every edge.
        let incompatible = expected_output
            .matches
            .iter()
            .filter(|m| m.type_compatible == Some(false))
            .count();
        assert_eq!(incompatible, 4, "4 deliberately-incompatible edges");
        assert!(
            expected_output
                .matches
                .iter()
                .all(|m| m.type_compatible.is_some()),
            "every corpus edge labels a compat verdict"
        );

        // Fan-out: two producers (subscriber repos) on one pubsub key.
        let fanout = expected_output
            .matches
            .iter()
            .filter(|m| m.producer_key == "pubsub|catalog.price.updated")
            .count();
        assert_eq!(
            fanout, 2,
            "catalog.price.updated fans out to two subscribers"
        );

        // 7 orphans: 5 producers + 2 consumers; exactly one roadmap (SQS digest).
        assert_eq!(expected_output.orphans.len(), 7);
        let roadmap_orphans: Vec<_> = expected_output
            .orphans
            .iter()
            .filter(|o| o.tier == TIER_ROADMAP)
            .collect();
        assert_eq!(roadmap_orphans.len(), 1);
        assert_eq!(roadmap_orphans[0].key, "pubsub|notifications.digest");

        // The Implicit inference case (support-desk GET /tickets/:id).
        let any_implicit = repo_expected.iter().any(|(_r, e)| {
            e.endpoints
                .iter()
                .any(|ep| ep.type_state.as_deref() == Some("Implicit"))
        });
        assert!(any_implicit, "corpus-3 keeps an Implicit type_state case");

        // 3 formal HTTP-keyed decoys (supertest, msw, self-call).
        let n_decoys: usize = repo_expected
            .iter()
            .map(|(_r, e)| e.must_not_emit.len())
            .sum();
        assert_eq!(n_decoys, 3, "3 _must_not_emit decoys across the corpus");

        // Dependency conflict: zod 3.23.0 vs 4.0.0, critical, used in code.
        assert_eq!(expected_output.dependency_conflicts.len(), 1);
        let dc = &expected_output.dependency_conflicts[0];
        assert_eq!(dc.package, "zod");
        assert_eq!(dc.severity, "critical");
    }

    #[test]
    fn empty_projection_against_real_corpus_scores_zero_recall() {
        let corpus = corpus_dir();
        let repos = discover_repos(&corpus);
        let repo_expected = load_repo_expected(&repos);
        let expected_output = load_expected_output(&corpus);

        // The empty projection has no compat verdicts, so the §7 guard trips —
        // that is the correct loud signal (compat data is absent). Post-#226 the
        // guard no longer panics inside the scorer: it rides `score.compat`, and
        // the FULL vector is still computed. Verify the guard verdict, then score
        // over a projection that DOES carry a verdict so the other metrics'
        // zero-recall behaviour is observable.
        let aliases = load_repo_aliases(&repos);
        let empty = empty_proj();
        assert!(
            score_corpus(&empty, &repo_expected, &expected_output, &aliases)
                .compat
                .is_err(),
            "empty projection → §7 guard trips (no compat verdict present)"
        );

        // A projection with one (irrelevant) verdict-bearing edge clears the
        // guard; everything else is still empty → recall 0 on the real metrics.
        let mut proj = empty_proj();
        proj.cross_repo_matches.push(cm_compat(
            "x",
            "http|GET|/nope",
            "y",
            "http|GET|/nope",
            Some(true),
        ));
        let score = score_corpus(&proj, &repo_expected, &expected_output, &aliases);
        assert!(
            score.compat.is_ok(),
            "one verdict-bearing edge clears the §7 guard"
        );
        let cap = score.by_tier.get(TIER_CAPABILITY).unwrap();
        assert_eq!(cap.ep_prf.1, 0.0, "no endpoints found → recall 0");
        assert_eq!(cap.ep_prf.0, 0.0, "found==0 with expected>0 → precision 0");
        assert_eq!(cap.match_prf.1, 0.0, "no real matches found → recall 0");
        assert_eq!(cap.dep_prf.1, 0.0, "no deps found → recall 0");
    }

    /// #431 repro: an OSS slice labels its edges by corpus repo DIR name
    /// (`platform`, `embed-kit`, `sdk`) while the projection carries the
    /// carrick.json SERVICE names (`webapp`, `rpc`, `embed-core` — the #368
    /// `service_name ?? repo_name` convention). The edges are byte-identical on
    /// every key segment; only the repo-identity vocabulary differs. The scorer
    /// must fold service names to their owning corpus repo dir before the row-7
    /// join, so these score as the perfect matches they are — not the silent
    /// 0.00 the recorded run printed.
    #[test]
    fn match_join_folds_service_names_to_corpus_repo_dirs() {
        let repo_expected: Vec<(String, ExpectedRepo)> = vec![];
        let expected_output: ExpectedOutput = serde_json::from_value(serde_json::json!({
            "matches": [
                { "producer_repo": "platform", "producer_key": "http|POST|/api/v2/embed/token",
                  "consumer_repo": "embed-kit", "consumer_key": "http|POST|/api/v2/embed/token",
                  "protocol": "http", "tier": "capability" },
                { "producer_repo": "embed-kit", "producer_key": "pubsub|document-completed",
                  "consumer_repo": "platform", "consumer_key": "pubsub|document-completed",
                  "protocol": "pubsub", "tier": "capability" },
                { "producer_repo": "platform", "producer_key": "http|GET|/envelope/:param",
                  "consumer_repo": "sdk", "consumer_key": "http|GET|/envelope/:param",
                  "protocol": "http", "tier": "capability" }
            ]
        }))
        .unwrap();
        // The projection's edges: same keys, service-name repo identities. `sdk`
        // is a flat repo (no carrick.json service name) so it stays `sdk`.
        let mut proj = empty_proj();
        proj.cross_repo_matches = vec![
            cm(
                "webapp",
                "http|POST|/api/v2/embed/token",
                "embed-core",
                "http|POST|/api/v2/embed/token",
            ),
            cm(
                "embed-core",
                "pubsub|document-completed",
                "rpc",
                "pubsub|document-completed",
            ),
            cm(
                "webapp",
                "http|GET|/envelope/:param",
                "sdk",
                "http|GET|/envelope/:param",
            ),
        ];

        // The fold, as `load_repo_aliases` would build it from the two
        // monorepos' carrick.json files.
        let aliases: std::collections::HashMap<String, String> = [
            ("webapp", "platform"),
            ("rpc", "platform"),
            ("embed-core", "embed-kit"),
        ]
        .into_iter()
        .map(|(s, r)| (s.to_string(), r.to_string()))
        .collect();

        // WITHOUT the fold: the recorded bug — 0.00 across the board despite
        // byte-identical keys, and the zero-join diagnosis fires loud.
        let unfolded = score_corpus(
            &proj,
            &repo_expected,
            &expected_output,
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            unfolded.by_tier.get(TIER_CAPABILITY).unwrap().match_prf,
            (0.0, 0.0, 0.0),
            "the unfolded join reproduces the recorded 0.00"
        );
        assert!(
            unfolded.match_zero_join.is_some(),
            "labels + predictions present with zero joins must trip the #431 diagnosis"
        );

        // WITH the fold: the same edges join perfectly.
        let score = score_corpus(&proj, &repo_expected, &expected_output, &aliases);
        let cap = score.by_tier.get(TIER_CAPABILITY).unwrap();
        assert_eq!(
            cap.match_prf,
            (1.0, 1.0, 1.0),
            "identical edges must join across the repo-name/service-name vocabularies (#431)"
        );
        assert!(score.match_zero_join.is_none());
        assert!(
            score.compat.is_ok(),
            "no labelled verdicts → §7 guard vacuously clean (compat is UNSCORED, not red)"
        );
    }

    /// #431: the fold is built from the committed corpus carrick.json files —
    /// corpus-1's orders-monorepo declares `orders-pkg` + `gateway`, the flat
    /// repos declare no service names.
    #[test]
    fn load_repo_aliases_reads_corpus_carrick_json() {
        if corpus_name() != "xrepo-corpus-1" {
            return;
        }
        let repos = discover_repos(&corpus_dir());
        let aliases = load_repo_aliases(&repos);
        assert_eq!(
            aliases.get("orders-pkg").map(String::as_str),
            Some("orders-monorepo")
        );
        assert_eq!(
            aliases.get("gateway").map(String::as_str),
            Some("orders-monorepo")
        );
        assert_eq!(
            aliases.len(),
            2,
            "flat repos contribute no aliases, got {aliases:?}"
        );
    }

    /// The #431 zero-join diagnosis is precise: it stays silent when either
    /// side is empty (plain recall failure / unlabelled slice) or when at least
    /// one edge joins, and fires only on the both-present-zero-overlap
    /// signature.
    #[test]
    fn match_join_diagnosis_fires_only_on_disjoint_nonempty_sides() {
        let expected = vec![ExpMatch {
            producer_repo: "platform".to_string(),
            producer_key: "http|GET|/a".to_string(),
            consumer_repo: "sdk".to_string(),
            consumer_key: "http|GET|/a".to_string(),
            type_compatible: None,
            tier: TIER_CAPABILITY.to_string(),
        }];
        // Empty found: silent (an ordinary recall miss, not a join defect).
        assert!(match_join_diagnosis(&expected, &[]).is_none());
        // Empty labels: silent (nothing to join).
        assert!(match_join_diagnosis(&[], &[cm("x", "http|GET|/a", "y", "http|GET|/a")]).is_none());
        // One edge joins: silent even with extra unjoined edges present.
        let partly = vec![
            cm("platform", "http|GET|/a", "sdk", "http|GET|/a"),
            cm("webapp", "http|GET|/b", "sdk", "http|GET|/b"),
        ];
        assert!(match_join_diagnosis(&expected, &partly).is_none());
        // Both sides present, zero overlap: fires, naming both vocabularies.
        let disjoint = vec![cm("webapp", "http|GET|/a", "sdk", "http|GET|/a")];
        let msg = match_join_diagnosis(&expected, &disjoint)
            .expect("disjoint non-empty sides must fire the diagnosis");
        assert!(
            msg.contains("platform") && msg.contains("webapp"),
            "the diagnosis names both repo vocabularies, got: {msg}"
        );
    }

    /// #431: a joined edge whose LABEL carries no verdict is compat-unlabelled —
    /// it must neither score nor ding the compat row (partial-label OSS slices
    /// label matches without compat). Verdict-labelled edges still score.
    #[test]
    fn compat_verdict_skips_unlabelled_edges() {
        let expected = vec![
            ExpMatch {
                producer_repo: "a".to_string(),
                producer_key: "http|GET|/x".to_string(),
                consumer_repo: "b".to_string(),
                consumer_key: "http|GET|/x".to_string(),
                type_compatible: Some(true),
                tier: TIER_CAPABILITY.to_string(),
            },
            ExpMatch {
                producer_repo: "a".to_string(),
                producer_key: "http|GET|/y".to_string(),
                consumer_repo: "b".to_string(),
                consumer_key: "http|GET|/y".to_string(),
                type_compatible: None, // compat-unlabelled edge
                tier: TIER_CAPABILITY.to_string(),
            },
        ];
        let found = vec![
            cm_compat("a", "http|GET|/x", "b", "http|GET|/x", Some(true)),
            // Joined, but its label is compat-unlabelled → not scored, so the
            // actual None cannot read as a miss.
            cm("a", "http|GET|/y", "b", "http|GET|/y"),
        ];
        assert!(
            (score_compat_verdict_accuracy(&expected, &found, TIER_CAPABILITY) - 1.0).abs() < 1e-9,
            "the unlabelled edge must not ding the verdict-labelled edge's 1.0"
        );
    }

    #[test]
    fn eval_run_record_round_trips_full_vector() {
        let rec = EvalRunRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            ts_unix: 1_700_000_000,
            scanner_version: "0.1.40".into(),
            carrick_sha: "local".into(),
            github_run_id: None,
            fixture: corpus_name(),
            runs_requested: 5,
            runs_effective: 5,
            model_id: None,
            prompt_hash: None,
            ep_precision_mean: 0.8,
            ep_precision_sd: 0.1,
            ep_recall_mean: 0.7,
            ep_recall_sd: 0.12,
            ep_f1_mean: 0.74,
            ep_f1_sd: 0.11,
            ep_pass_at_k: 0.0,
            ep_pass_pow_k: 0.0,
            call_precision_mean: 0.9,
            call_precision_sd: 0.05,
            call_recall_mean: 0.85,
            call_recall_sd: 0.06,
            call_f1_mean: 0.87,
            call_f1_sd: 0.05,
            call_pass_at_k: 0.0,
            call_pass_pow_k: 0.0,
            note: Some("S4 full vector".into()),
            tier: Some("xrepo".into()),
            corpus: Some(corpus_name()),
            match_precision_mean: Some(0.6),
            match_precision_sd: Some(0.2),
            match_recall_mean: Some(0.5),
            match_recall_sd: Some(0.15),
            match_f1_mean: Some(0.55),
            match_f1_sd: Some(0.17),
            type_anchor_accuracy_mean: Some(0.9),
            type_anchor_accuracy_sd: Some(0.0),
            type_resolution_accuracy_mean: Some(0.8),
            type_resolution_accuracy_sd: Some(0.0),
            compat_verdict_accuracy_mean: Some(1.0),
            compat_verdict_accuracy_sd: Some(0.0),
            owner_accuracy_mean: Some(0.75),
            owner_accuracy_sd: Some(0.0),
            decoy_leak_mean: Some(0.0),
            decoy_leak_sd: Some(0.0),
            dep_f1_mean: Some(1.0),
            dep_f1_sd: Some(0.0),
        };
        let line = serde_json::to_string(&rec).unwrap();
        let back: EvalRunRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back.tier.as_deref(), Some("xrepo"));
        assert_eq!(back.corpus.as_deref(), Some(corpus_name().as_str()));
        assert_eq!(back.match_f1_mean, Some(0.55));
        // The full vector now serializes (it is no longer None/omitted).
        assert_eq!(back.compat_verdict_accuracy_mean, Some(1.0));
        assert_eq!(back.type_anchor_accuracy_mean, Some(0.9));
        assert_eq!(back.dep_f1_mean, Some(1.0));
        assert!(line.contains("compat_verdict_accuracy_mean"));
        assert!(line.contains("dep_f1_mean"));
    }
}
