//! Tier-A extraction-quality scorer (evals plan, Slice 2).
//!
//! Runs the release scanner against each framework fixture **N times** (default 5,
//! override with `CARRICK_EVAL_RUNS`) in `CARRICK_OUTPUT_JSON` mode, scores each
//! run against the fixture's `expected.json`, and reports:
//!   - endpoint & call precision/recall/F1 as **mean ± stddev** across runs, and
//!   - **pass@k / pass^k** per ground-truth element — found in *at least one* run
//!     (capability ceiling) vs found in *all* runs (consistency). The pass@k −
//!     pass^k gap is the run-to-run wobble, made explicit.
//!
//! **Report-only:** asserts only a non-flaky floor (≥1 run parsed + endpoint
//! pass@k > 0), because the live LLM is nondeterministic and a strict assertion
//! would flake. A transient failed run is skipped (effective n is reported), not
//! fatal unless *every* run for a fixture fails.
//!
//! Auth is GitHub Actions OIDC — the scanner is keyless (no API-key path). The
//! runner exposes `ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN` only with
//! `permissions: id-token: write`; locally (no OIDC) this test skips cleanly.
//! Runner: `.github/workflows/eval-tier-a.yml`.
//!
//! The API endpoint is compile-time (`build.rs`), so build with it set:
//!   CARRICK_API_ENDPOINT="https://api.carrick.tools" cargo build --release
//!   cargo test --release --test eval_tier_a -- --test-threads=1 --nocapture

use carrick::eval_output::EvalProjection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FIXTURES: &[&str] = &["koa-api", "fastify-api", "hapi-api", "nestjs-api"];
const DEFAULT_RUNS: usize = 5;

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    endpoints: Vec<ExpEndpoint>,
    #[serde(default)]
    calls: Vec<ExpCall>,
}

#[derive(Debug, Deserialize)]
struct ExpEndpoint {
    method: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct ExpCall {
    method: String,
    host_contains: String,
    path_contains: String,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collapse `:x` / `{x}` / `[x]` param syntaxes to `:param` and strip a trailing
/// slash, so the Hapi `{id}` vs Express-style `:id` split (issue #167) doesn't
/// cause false mismatches.
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

/// Mean and sample stddev (n-1). stddev is 0.0 for n < 2.
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

/// Run the scanner in JSON mode and return the parsed projection, retrying once
/// on a non-result. A "non-result" is empty stdout OR a parsed-but-empty
/// projection (zero endpoints AND zero calls): for these fixtures every scan
/// should extract something, so an empty extraction is a transient dead scan
/// (cloud/LLM hiccup) — retry it rather than score it as a catastrophic zero
/// (which at low N forces pass^k to 0). The OIDC env
/// (`ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN`) is inherited from the runner; the
/// API endpoint is compile-time.
///
/// `dump_dir`, when set (capture mode), is passed to the scanner as
/// `CARRICK_EVAL_DUMP_DIR` so the file-analyzer persists its raw input/output
/// there for prompt-hardening diagnostics.
///
/// Each scan runs HERMETIC: `CARRICK_LOCAL_STORAGE_DIR` (a per-scan temp dir)
/// selects `LocalDirStorage` over the real cloud store, and
/// `CARRICK_LOCAL_STORAGE_ISOLATE=1` makes sibling-repo download return empty
/// (the xrepo harness's Phase-A isolation). Without this, the scan joins
/// whatever the CI identity's live project holds — when dogfood scans populated
/// that project, every fixture's projection inherited its endpoints and calls,
/// collapsing precision while recall stayed 1.0 (#505). The LLM extraction
/// path is unaffected: file-analyzer calls go to the compile-time API endpoint
/// regardless of the storage backend.
fn run_scanner(bin: &Path, fixture_dir: &Path, dump_dir: Option<&Path>) -> Option<EvalProjection> {
    for attempt in 1..=2 {
        let hermetic_store = tempfile::tempdir().expect("create hermetic storage dir");
        let mut cmd = Command::new(bin);
        cmd.arg(fixture_dir)
            .env("CARRICK_OUTPUT_JSON", "1")
            .env("CARRICK_LOCAL_STORAGE_DIR", hermetic_store.path())
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
        if let Some(d) = dump_dir {
            cmd.env("CARRICK_EVAL_DUMP_DIR", d);
        }
        let output = cmd.output().expect("failed to spawn the carrick binary");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if let Some(proj) = parse_projection(&stdout)
            && (!proj.endpoints.is_empty() || !proj.calls.is_empty())
        {
            return Some(proj);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(15)..].join("\n");
        eprintln!(
            "[eval] no usable extraction for {} (attempt {}/2): {}\n--- stderr (last 15 lines) ---\n{}",
            fixture_dir.display(),
            attempt,
            output.status,
            tail
        );
    }
    None
}

/// Tolerate log noise around the JSON by slicing from the first `{` to the last `}`.
fn parse_projection(stdout: &str) -> Option<EvalProjection> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    serde_json::from_str(stdout.get(start..=end)?).ok()
}

/// One run's scores plus per-ground-truth-element hit flags (for pass@k/pass^k).
struct RunScore {
    ep_prf: (f64, f64, f64),
    call_prf: (f64, f64, f64),
    ep_hits: Vec<bool>,
    call_hits: Vec<bool>,
}

fn score_run(
    proj: &EvalProjection,
    expected_eps: &[(String, String)],
    expected: &Expected,
) -> RunScore {
    // Endpoints: set comparison over (METHOD, normalised path), HTTP only.
    let expected_set: HashSet<(String, String)> = expected_eps.iter().cloned().collect();
    let found_eps: HashSet<(String, String)> = proj
        .endpoints
        .iter()
        .filter(|o| o.protocol == "http")
        .filter_map(|o| {
            Some((
                o.method.clone()?.to_uppercase(),
                norm_path(o.path.as_deref()?),
            ))
        })
        .collect();
    let ep_tp = expected_set.intersection(&found_eps).count();
    let ep_prf = prf(ep_tp, found_eps.len(), expected_set.len());
    let ep_hits: Vec<bool> = expected_eps.iter().map(|e| found_eps.contains(e)).collect();

    // Calls: fuzzy match on method + host_contains + path_contains, HTTP only.
    // `path` is the canonical MATCH key and is host-free for literal-origin
    // URLs (`consumer_call_path` strips `scheme://host`), so `host_contains`
    // is checked against the raw source target (`target_url`) with the
    // canonical path as fallback — the host would otherwise be unmatchable
    // by construction and every host-asserting label would MISS.
    let found_calls: Vec<(String, String, String)> = proj
        .calls
        .iter()
        .filter(|o| o.protocol == "http")
        .filter_map(|o| {
            let path = o.path.clone()?;
            let raw = o.target_url.clone().unwrap_or_else(|| path.clone());
            Some((
                o.method.clone().unwrap_or_default().to_uppercase(),
                path,
                raw,
            ))
        })
        .collect();
    let matches = |e: &ExpCall, c: &(String, String, String)| {
        c.0 == e.method.to_uppercase()
            && (c.1.contains(&e.host_contains) || c.2.contains(&e.host_contains))
            && (c.1.contains(&e.path_contains) || c.2.contains(&e.path_contains))
    };
    let call_hits: Vec<bool> = expected
        .calls
        .iter()
        .map(|e| found_calls.iter().any(|c| matches(e, c)))
        .collect();
    let matched_expected = call_hits.iter().filter(|h| **h).count();
    let matched_found = found_calls
        .iter()
        .filter(|c| expected.calls.iter().any(|e| matches(e, c)))
        .count();
    let call_p = if found_calls.is_empty() {
        if expected.calls.is_empty() { 1.0 } else { 0.0 }
    } else {
        matched_found as f64 / found_calls.len() as f64
    };
    let call_r = if expected.calls.is_empty() {
        1.0
    } else {
        matched_expected as f64 / expected.calls.len() as f64
    };
    let call_f1 = if call_p + call_r == 0.0 {
        0.0
    } else {
        2.0 * call_p * call_r / (call_p + call_r)
    };

    RunScore {
        ep_prf,
        call_prf: (call_p, call_r, call_f1),
        ep_hits,
        call_hits,
    }
}

/// pass@k (any run) and pass^k (all runs) over a set of per-element hit columns.
/// Returns (pass_at_k, pass_pow_k); both 1.0 when there are no elements.
fn pass_rates(hits_per_run: &[Vec<bool>], num_elems: usize) -> (f64, f64) {
    if num_elems == 0 {
        return (1.0, 1.0);
    }
    let n = hits_per_run.len();
    let mut any = 0usize;
    let mut all = 0usize;
    for i in 0..num_elems {
        let hits = hits_per_run.iter().filter(|run| run[i]).count();
        if hits >= 1 {
            any += 1;
        }
        if hits == n {
            all += 1;
        }
    }
    (any as f64 / num_elems as f64, all as f64 / num_elems as f64)
}

/// Schema version of an [`EvalRunRecord`]. Bump on any breaking field change so a
/// reader can reject a stale baseline rather than silently mis-compare.
const RECORD_SCHEMA_VERSION: u32 = 1;

/// One scored fixture from one scorer invocation — the longitudinal-store unit
/// (evals plan, Slice 3 / Layer 3). Written as one JSON line per fixture so the
/// store is diffable and a release-over-release comparison is a set operation,
/// not an eyeballed delta against `index-quality-log.md`.
///
/// `model_id`/`prompt_hash` are the **attribution linchpin** but are not
/// observable scanner-side: post-Vertex the model + prompt live in the lambda,
/// and the deployed prompt can differ from any checked-out copy. They stay `None`
/// until the cloud echoes provenance in its response (Slice 3b, tracked as a
/// `carrick-cloud` follow-up); the fields exist now so the schema is stable when
/// it lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvalRunRecord {
    schema_version: u32,
    /// Unix epoch seconds — orders runs without pulling in a date crate.
    ts_unix: u64,
    /// The scoring binary's own version (`env!("CARGO_PKG_VERSION")`).
    scanner_version: String,
    /// Commit under test: `GITHUB_SHA` in CI, else `"local"`.
    carrick_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github_run_id: Option<String>,
    fixture: String,
    runs_requested: usize,
    runs_effective: usize,
    // --- attribution provenance (Slice 3b; cloud-echoed, None today) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_hash: Option<String>,
    // --- endpoint metrics (mean ± sample sd across effective runs) ---
    ep_precision_mean: f64,
    ep_precision_sd: f64,
    ep_recall_mean: f64,
    ep_recall_sd: f64,
    ep_f1_mean: f64,
    ep_f1_sd: f64,
    ep_pass_at_k: f64,
    ep_pass_pow_k: f64,
    // --- call metrics ---
    call_precision_mean: f64,
    call_precision_sd: f64,
    call_recall_mean: f64,
    call_recall_sd: f64,
    call_f1_mean: f64,
    call_f1_sd: f64,
    call_pass_at_k: f64,
    call_pass_pow_k: f64,
    /// Set on hand-seeded baseline rows whose P/R are approximated from a reported
    /// F1; cleared once a real workflow run re-pins the baseline. Informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    // --- cross-repo metrics (Option; populated by S4/eval_xrepo, None for Tier-A) ---
    /// Eval tier: "A" for Tier-A fixtures, "xrepo" for the cross-repo corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tier: Option<String>,
    /// Corpus identifier (e.g. "xrepo-corpus-1"). None for Tier-A.
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

/// Load the committed baseline (`tests/eval/baseline.jsonl`), indexed by fixture
/// (last row wins). Returns an empty map if the file is absent — the first run
/// then seeds the comparison instead of failing.
fn load_baseline(path: &Path) -> std::collections::HashMap<String, EvalRunRecord> {
    let mut by_fixture = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return by_fixture;
    };
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        match serde_json::from_str::<EvalRunRecord>(line) {
            Ok(rec) if rec.schema_version == RECORD_SCHEMA_VERSION => {
                by_fixture.insert(rec.fixture.clone(), rec);
            }
            // Reject a stale baseline rather than silently mis-comparing against an
            // old schema — the whole point of RECORD_SCHEMA_VERSION.
            Ok(rec) => eprintln!(
                "[eval] baseline.jsonl line {}: schema_version {} != {RECORD_SCHEMA_VERSION} — \
                 skipped (re-pin the baseline)",
                i + 1,
                rec.schema_version
            ),
            Err(e) => eprintln!(
                "[eval] baseline.jsonl line {}: unparseable ({e}) — skipped",
                i + 1
            ),
        }
    }
    by_fixture
}

/// A metric regresses if it drops below baseline by more than the noise band —
/// the per-fixture stddev, floored at `MIN_BAND` so a deterministic (sd≈0) metric
/// still tolerates a hair of float wobble. This is the cheap, honest form of the
/// plan's paired-difference test; clustered error bars / a formal paired-t are a
/// later refinement (tracked with Slice 3b).
const MIN_BAND: f64 = 0.05;

fn regressed(current: f64, baseline: f64, noise_sd: f64) -> bool {
    current < baseline - noise_sd.max(MIN_BAND)
}

/// Print a release-over-release comparison of the current records against the
/// baseline. Report-only: flags regressions, asserts nothing (the live LLM is
/// nondeterministic; a hard gate here would flake — see the plan's gating rule).
fn compare_to_baseline(
    records: &[EvalRunRecord],
    baseline: &std::collections::HashMap<String, EvalRunRecord>,
) {
    if baseline.is_empty() {
        println!(
            "\n[eval] no baseline at tests/eval/baseline.jsonl — this run would seed it. \
             Skipping comparison."
        );
        return;
    }
    println!("\n=== Release comparison vs baseline (band = max(sd, {MIN_BAND:.2})) ===");
    println!("fixture       metric        baseline  current     Δ    flag");
    let mut any_regression = false;
    for rec in records {
        let Some(base) = baseline.get(&rec.fixture) else {
            println!("{:<13} (no baseline row — new fixture)", rec.fixture);
            continue;
        };
        // (label, current mean, current sd, baseline mean, baseline sd). The noise
        // band is max(current_sd, baseline_sd) so it reflects BOTH runs' observed
        // variance, not just this run's sample (pass^k has no sd of its own, so it
        // borrows the F1 sd as a proxy).
        let rows = [
            (
                "ep F1",
                rec.ep_f1_mean,
                rec.ep_f1_sd,
                base.ep_f1_mean,
                base.ep_f1_sd,
            ),
            (
                "ep pass^k",
                rec.ep_pass_pow_k,
                rec.ep_f1_sd,
                base.ep_pass_pow_k,
                base.ep_f1_sd,
            ),
            (
                "call F1",
                rec.call_f1_mean,
                rec.call_f1_sd,
                base.call_f1_mean,
                base.call_f1_sd,
            ),
            (
                "call pass^k",
                rec.call_pass_pow_k,
                rec.call_f1_sd,
                base.call_pass_pow_k,
                base.call_f1_sd,
            ),
        ];
        for (label, cur, cur_sd, base_val, base_sd) in rows {
            let delta = cur - base_val;
            let flag = if regressed(cur, base_val, cur_sd.max(base_sd)) {
                any_regression = true;
                "REGRESSED"
            } else if delta > MIN_BAND {
                "improved"
            } else {
                ""
            };
            println!(
                "{:<13} {:<12} {:>7.2}  {:>7.2}  {:>+6.2}  {}",
                rec.fixture, label, base_val, cur, delta, flag
            );
        }
    }
    if any_regression {
        println!(
            "\n[eval] ⚠ one or more metrics regressed beyond the noise band. Report-only — \
             investigate before the next release (prompt vs scanner: see attribution table)."
        );
    } else {
        println!("\n[eval] no metric regressed beyond the noise band.");
    }
    println!("=== end comparison ===");
}

/// Serialise the run's records to JSONL, write them to `target/eval-runs/` for the
/// workflow to upload as an artifact, and echo them to stdout between markers so
/// they are greppable straight from the CI step log.
fn emit_records(records: &[EvalRunRecord]) {
    let jsonl: String = records
        .iter()
        .map(|r| serde_json::to_string(r).expect("serialise EvalRunRecord"))
        .collect::<Vec<_>>()
        .join("\n");

    let out_dir = manifest_dir().join("target/eval-runs");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("[eval] could not create {}: {e}", out_dir.display());
    } else {
        let ts = records.first().map(|r| r.ts_unix).unwrap_or(0);
        let path = out_dir.join(format!("run-{ts}.jsonl"));
        match std::fs::File::create(&path).and_then(|mut f| writeln!(f, "{jsonl}")) {
            Ok(()) => println!(
                "\n[eval] wrote {} record(s) to {}",
                records.len(),
                path.display()
            ),
            Err(e) => eprintln!("[eval] could not write {}: {e}", path.display()),
        }
    }

    println!("=== EVAL JSONL BEGIN ===");
    println!("{jsonl}");
    println!("=== EVAL JSONL END ===");
}

#[test]
fn tier_a_extraction_quality() {
    // The scanner authenticates only via GitHub Actions OIDC. Without a runner
    // granting `id-token: write`, no real scan can run — skip cleanly.
    if std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").is_err() {
        eprintln!(
            "[eval] GitHub Actions OIDC unavailable — skipping Tier-A scorer \
             (run in CI with `permissions: id-token: write`)."
        );
        return;
    }

    let bin = manifest_dir().join("target/release/carrick");
    if !bin.exists() {
        eprintln!(
            "[eval] release binary missing at {} — build first: \
             CARRICK_API_ENDPOINT=\"https://api.carrick.tools\" cargo build --release",
            bin.display()
        );
        return;
    }

    let runs_n: usize = std::env::var("CARRICK_EVAL_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&v| v >= 1)
        .unwrap_or(DEFAULT_RUNS);

    println!("\n=== Tier-A extraction quality (evals Slice 2, N={runs_n}) ===");
    println!("(P/R/F1 as mean±sd across runs; pass@k = found in any run, pass^k = found in all)\n");

    // Accumulators for the cross-fixture MEAN row.
    let nf = FIXTURES.len() as f64;
    let (mut sum_ep_f1, mut sum_call_f1) = (0.0, 0.0);
    let (mut sum_ep_passpow, mut sum_call_passpow) = (0.0, 0.0);

    // Provenance, stamped once per invocation onto every fixture's record.
    let ts_unix = now_unix();
    let scanner_version = env!("CARGO_PKG_VERSION").to_string();
    let carrick_sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local".to_string());
    let github_run_id = std::env::var("GITHUB_RUN_ID").ok();
    // Capture mode (opt-in): dump each run's raw file-analyzer input/output under
    // target/eval-runs/dump/<fixture>/run<k> for prompt-hardening diagnosis.
    let capture = std::env::var("CARRICK_EVAL_CAPTURE")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let mut records: Vec<EvalRunRecord> = Vec::new();

    for name in FIXTURES {
        let fixture_dir = manifest_dir().join("tests/fixtures").join(name);
        let expected: Expected = serde_json::from_str(
            &std::fs::read_to_string(fixture_dir.join("expected.json"))
                .unwrap_or_else(|e| panic!("read expected.json for {name}: {e}")),
        )
        .unwrap_or_else(|e| panic!("parse expected.json for {name}: {e}"));

        let expected_eps: Vec<(String, String)> = expected
            .endpoints
            .iter()
            .map(|e| (e.method.to_uppercase(), norm_path(&e.path)))
            .collect();

        // N runs; skip a transient failure, keep the rest.
        let mut scores: Vec<RunScore> = Vec::new();
        for run_idx in 1..=runs_n {
            let dump_dir = capture.then(|| {
                manifest_dir()
                    .join("target/eval-runs/dump")
                    .join(name)
                    .join(format!("run{run_idx}"))
            });
            match run_scanner(&bin, &fixture_dir, dump_dir.as_deref()) {
                Some(proj) => scores.push(score_run(&proj, &expected_eps, &expected)),
                None => eprintln!(
                    "[eval] {name}: run {run_idx}/{runs_n} produced no usable extraction — skipped"
                ),
            }
        }
        let n = scores.len();
        assert!(n > 0, "[eval] {name}: all {runs_n} runs failed (cloud/LLM)");

        let (ep_p, ep_p_sd) = mean_sd(&scores.iter().map(|s| s.ep_prf.0).collect::<Vec<_>>());
        let (ep_r, ep_r_sd) = mean_sd(&scores.iter().map(|s| s.ep_prf.1).collect::<Vec<_>>());
        let (ep_f, ep_f_sd) = mean_sd(&scores.iter().map(|s| s.ep_prf.2).collect::<Vec<_>>());
        let (cl_p, cl_p_sd) = mean_sd(&scores.iter().map(|s| s.call_prf.0).collect::<Vec<_>>());
        let (cl_r, cl_r_sd) = mean_sd(&scores.iter().map(|s| s.call_prf.1).collect::<Vec<_>>());
        let (cl_f, cl_f_sd) = mean_sd(&scores.iter().map(|s| s.call_prf.2).collect::<Vec<_>>());

        let ep_hits: Vec<Vec<bool>> = scores.iter().map(|s| s.ep_hits.clone()).collect();
        let call_hits: Vec<Vec<bool>> = scores.iter().map(|s| s.call_hits.clone()).collect();
        let (ep_at, ep_pow) = pass_rates(&ep_hits, expected_eps.len());
        let (cl_at, cl_pow) = pass_rates(&call_hits, expected.calls.len());

        let neff = if n < runs_n {
            format!(" (n={n}/{runs_n})")
        } else {
            String::new()
        };
        println!("{name}{neff}");
        println!(
            "  endpoints  P {ep_p:.2}±{ep_p_sd:.2}  R {ep_r:.2}±{ep_r_sd:.2}  F1 {ep_f:.2}±{ep_f_sd:.2}   pass@{n} {ep_at:.2}  pass^{n} {ep_pow:.2}"
        );
        println!(
            "  calls      P {cl_p:.2}±{cl_p_sd:.2}  R {cl_r:.2}±{cl_r_sd:.2}  F1 {cl_f:.2}±{cl_f_sd:.2}   pass@{n} {cl_at:.2}  pass^{n} {cl_pow:.2}"
        );

        // Per-element flicker breakdown: name *which* ground-truth element is
        // inconsistent — the actionable signal behind a sub-1.0 pass^k. Found in
        // some-but-not-all runs is a consistency problem (`flick`); found in zero
        // is a hard miss (`MISS`). A single run pinpoints the culprit, so a fix
        // can be targeted and its hit-rate measured before/after.
        for i in 0..expected_eps.len() {
            let c = ep_hits.iter().filter(|run| run[i]).count();
            if c < n {
                let (m, p) = &expected_eps[i];
                let kind = if c == 0 { "MISS " } else { "flick" };
                println!("  {kind} endpoint {m} {p}  ({c}/{n} runs)");
            }
        }
        for i in 0..expected.calls.len() {
            let c = call_hits.iter().filter(|run| run[i]).count();
            if c < n {
                let e = &expected.calls[i];
                let kind = if c == 0 { "MISS " } else { "flick" };
                println!(
                    "  {kind} call     {} host~{} path~{}  ({c}/{n} runs)",
                    e.method, e.host_contains, e.path_contains
                );
            }
        }

        sum_ep_f1 += ep_f;
        sum_call_f1 += cl_f;
        sum_ep_passpow += ep_pow;
        sum_call_passpow += cl_pow;

        records.push(EvalRunRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            ts_unix,
            scanner_version: scanner_version.clone(),
            carrick_sha: carrick_sha.clone(),
            github_run_id: github_run_id.clone(),
            fixture: name.to_string(),
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
            ep_pass_at_k: ep_at,
            ep_pass_pow_k: ep_pow,
            call_precision_mean: cl_p,
            call_precision_sd: cl_p_sd,
            call_recall_mean: cl_r,
            call_recall_sd: cl_r_sd,
            call_f1_mean: cl_f,
            call_f1_sd: cl_f_sd,
            call_pass_at_k: cl_at,
            call_pass_pow_k: cl_pow,
            note: None,
            tier: None,
            corpus: None,
            match_precision_mean: None,
            match_precision_sd: None,
            match_recall_mean: None,
            match_recall_sd: None,
            match_f1_mean: None,
            match_f1_sd: None,
            type_anchor_accuracy_mean: None,
            type_anchor_accuracy_sd: None,
            type_resolution_accuracy_mean: None,
            type_resolution_accuracy_sd: None,
            compat_verdict_accuracy_mean: None,
            compat_verdict_accuracy_sd: None,
            owner_accuracy_mean: None,
            owner_accuracy_sd: None,
            decoy_leak_mean: None,
            decoy_leak_sd: None,
            dep_f1_mean: None,
            dep_f1_sd: None,
        });

        // Non-flaky floor: at least one expected endpoint found in at least one run.
        assert!(
            ep_at > 0.0,
            "[eval] {name}: endpoint pass@k is 0 — no expected endpoint matched across \
             {n} run(s). Expected: {expected_eps:?}"
        );
    }

    println!(
        "\nMEAN (avg over {} fixtures; each pass^k is at that fixture's effective n — \
         see per-fixture rows for any reduced n)",
        FIXTURES.len()
    );
    println!(
        "      endpoints F1 {:.2}  pass^k {:.2}   |   calls F1 {:.2}  pass^k {:.2}",
        sum_ep_f1 / nf,
        sum_ep_passpow / nf,
        sum_call_f1 / nf,
        sum_call_passpow / nf
    );
    println!("=== end Tier-A ===\n");

    // Slice 3 / Layer 3: persist the run and compare it to the pinned baseline.
    emit_records(&records);
    let baseline = load_baseline(&manifest_dir().join("tests/eval/baseline.jsonl"));
    compare_to_baseline(&records, &baseline);
}

/// Local, OIDC-free coverage for the Slice 3 store + comparison logic (the scored
/// test above skips without a runner). Exercises serde round-trip, the baseline
/// loader's resilience to junk lines, and the regression flag's noise band.
#[test]
fn baseline_store_and_comparison_smoke() {
    let base = EvalRunRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        ts_unix: 1_700_000_000,
        scanner_version: "0.1.37".into(),
        carrick_sha: "local".into(),
        github_run_id: None,
        fixture: "koa-api".into(),
        runs_requested: 5,
        runs_effective: 5,
        model_id: None,
        prompt_hash: None,
        ep_precision_mean: 1.0,
        ep_precision_sd: 0.0,
        ep_recall_mean: 0.90,
        ep_recall_sd: 0.13,
        ep_f1_mean: 0.95,
        ep_f1_sd: 0.11,
        ep_pass_at_k: 1.0,
        ep_pass_pow_k: 0.75,
        call_precision_mean: 1.0,
        call_precision_sd: 0.0,
        call_recall_mean: 1.0,
        call_recall_sd: 0.0,
        call_f1_mean: 1.0,
        call_f1_sd: 0.0,
        call_pass_at_k: 1.0,
        call_pass_pow_k: 1.0,
        note: None,
        tier: None,
        corpus: None,
        match_precision_mean: None,
        match_precision_sd: None,
        match_recall_mean: None,
        match_recall_sd: None,
        match_f1_mean: None,
        match_f1_sd: None,
        type_anchor_accuracy_mean: None,
        type_anchor_accuracy_sd: None,
        type_resolution_accuracy_mean: None,
        type_resolution_accuracy_sd: None,
        compat_verdict_accuracy_mean: None,
        compat_verdict_accuracy_sd: None,
        owner_accuracy_mean: None,
        owner_accuracy_sd: None,
        decoy_leak_mean: None,
        decoy_leak_sd: None,
        dep_f1_mean: None,
        dep_f1_sd: None,
    };

    // Round-trips through JSON, and the loader tolerates blank/comment/garbage lines.
    let line = serde_json::to_string(&base).unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "carrick_eval_baseline_smoke.{}.jsonl",
        std::process::id()
    ));
    std::fs::write(&tmp, format!("// header\n\n{line}\nNOT JSON\n")).unwrap();
    let loaded = load_baseline(&tmp);
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(loaded.len(), 1, "one valid row survives the junk lines");
    let got = &loaded["koa-api"];
    assert!((got.ep_f1_mean - 0.95).abs() < 1e-9);
    assert!(
        got.model_id.is_none(),
        "provenance stays None until Slice 3b"
    );

    // Missing file → empty map (first run seeds, never panics).
    assert!(load_baseline(&std::env::temp_dir().join("carrick_no_such_baseline.jsonl")).is_empty());

    // Regression flag: a drop within the noise band is tolerated; beyond it fires.
    // ep F1 sd is 0.11 → band = max(0.11, 0.05) = 0.11.
    assert!(
        !regressed(0.90, 0.95, 0.11),
        "0.05 drop within the 0.11 band is fine"
    );
    assert!(
        regressed(0.70, 0.95, 0.11),
        "0.25 drop beyond the band regresses"
    );
    // A deterministic metric (sd 0) still gets the MIN_BAND floor.
    assert!(
        !regressed(0.97, 1.0, 0.0),
        "0.03 drop within the 0.05 floor is fine"
    );
    assert!(
        regressed(0.90, 1.0, 0.0),
        "0.10 drop beyond the floor regresses"
    );
}
