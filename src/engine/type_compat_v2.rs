//! v2 type-compat integration ("tsc as serializer / tsc as judge").
//!
//! Scan time: derive capture anchors from the SAME collected type requests
//! the v1 bundle path uses (byte-identical aliases; the alias contracts in
//! `file_orchestrator` are load-bearing and untouched), run `capture_v2`
//! through the sidecar's stdio seam, and store the resulting stub package on
//! `CloudRepoData` as the wire artifact.
//!
//! Check time: materialize every participating service's stub, build one
//! `CheckPairSpec` per matched (producer, consumer, type_kind) manifest pair
//! (porting the ts_check manifest-matcher semantics: method + route-aware
//! path match for HTTP, exact operation keys for socket/graphql/pubsub),
//! run `check_v2`, and map the four-bucket verdicts back onto pair outcomes
//! the analyzer joins to `CrossRepoMatch` edges by structured identity.
//! No verdict travels as a parsed human label anywhere on this path.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::repo_relative;
use crate::analyzer::PairCheckOutcome;
use crate::cloud_storage::{
    CAPTURE_ARTIFACT_VERSION, CaptureStubArtifact, CloudRepoData, ManifestRole, ManifestTypeKind,
    TypeManifestEntry,
};
use crate::operation::OperationKey;
use crate::services::TypeSidecar;
use crate::services::type_sidecar::{
    AnchorOrigin, CaptureAliasRecord, CaptureAnchor, CheckPairEndpoint, CheckPairSpec,
    CheckStubInput, InferKind, InferRequestItem, ManifestEntry, ProbeProtocol, ProbeTypeKind,
    RetypeItem, RetypeOutcome, RetypeVerdict, SymbolRequest, VerdictBucket, VerdictSide,
};

// ===========================================================================
// Scan time: capture
// ===========================================================================

/// One shared notion of a "disqualifying top type" in printed TypeScript
/// type text (adversarial-review finding 2, aligned with the capture
/// self-check's deep walk): `any` or `unknown` appearing as a TYPE token at
/// ANY position — the whole text, an element (`any[]`), a type argument
/// (`Promise<any>`, `Record<string, any>`), a member
/// (`{ metadata: any }`), or an index signature (`{ [k: string]: any }`).
/// Such text must never anchor a literal capture or count as a resolved
/// shape: `any` is bidirectionally assignable, so an arbitrary counterparty
/// shape would read compatible, and `unknown` is the scrubbers' failed-
/// inference placeholder.
///
/// String-literal types are stripped first (`{ kind: "any" }` is fine) and
/// property-NAME positions are excluded (`{ any: string }`, `{ any?: T }`).
/// The error direction is deliberate: a false positive only demotes toward
/// unverifiable; a false negative is a false-compatible.
pub(crate) fn contains_disqualifying_top_type(text: &str) -> bool {
    let scrubbed = strip_string_literal_contents(text);
    let bytes = scrubbed.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    for keyword in ["any", "unknown"] {
        let mut search_from = 0;
        while let Some(pos) = scrubbed[search_from..].find(keyword) {
            let begin = search_from + pos;
            let end = begin + keyword.len();
            search_from = begin + 1;
            // Identifier boundaries (`company`, `unknownField`, `Anything`).
            if begin > 0 && is_ident(bytes[begin - 1]) {
                continue;
            }
            if end < bytes.len() && is_ident(bytes[end]) {
                continue;
            }
            // Property-name position: printers emit `name: T` / `name?: T`
            // with the colon immediately after the name.
            let rest = &bytes[end..];
            let rest = if rest.first() == Some(&b'?') {
                &rest[1..]
            } else {
                rest
            };
            if rest.first() == Some(&b':') {
                continue;
            }
            return true;
        }
    }
    false
}

// ===========================================================================
// Scan time: receiver classification (carrick#695)
// ===========================================================================

/// What a call site's RECEIVER says the site is.
///
/// A member call whose first argument is a route-shaped literal —
/// `x.verb("/lit", arg)` — states a path and a verb and no role: the same
/// shape registers a route (`app.get`) and requests one (`client.get`). The
/// ruling of 2026-09-05 forbids deciding that from the call's shape (a
/// trailing function, an options bag and a verb name each belong equally to
/// both). What decides it is what `x` IS, which the compiler knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiverRole {
    /// An instance of a package the framework detection named a server
    /// framework: the site registers a route.
    Server,
    /// An instance of a package the framework detection named a data fetcher:
    /// the site issues a request.
    Client,
}

/// Map a receiver type, as the sidecar resolved it, onto a role.
///
/// Three facts, in order, and no shape rule anywhere:
///  1. a receiver whose printed type carries `any`/`unknown` resolved to
///     nothing — on a checkout with no installed dependencies that is EVERY
///     dependency-typed receiver — so it states no role;
///  2. a receiver the workspace itself declares carries no package and stays
///     the model's (a locally written wrapper class can be either side);
///  3. otherwise the declaring package decides, against the lists the cloud's
///     framework detection produced for this repo. A package in both lists
///     (a full-stack package that ships a server and a fetcher) is ambiguous
///     and states nothing.
pub(crate) fn classify_receiver(
    type_string: &str,
    declaring_package: Option<&str>,
    frameworks: &[String],
    data_fetchers: &[String],
) -> Option<ReceiverRole> {
    if contains_disqualifying_top_type(type_string) {
        return None;
    }
    let package = normalize_declaring_package(declaring_package?);
    let names_it = |list: &[String]| {
        list.iter()
            .any(|entry| entry.trim().eq_ignore_ascii_case(&package))
    };
    match (names_it(frameworks), names_it(data_fetchers)) {
        (true, false) => Some(ReceiverRole::Server),
        (false, true) => Some(ReceiverRole::Client),
        _ => None,
    }
}

/// A DefinitelyTyped package names the package it types, and detection names
/// the runtime dependency: `@types/express` declares `express`'s types, and
/// `@types/hapi__hapi` declares `@hapi/hapi`'s. Unwrap that one convention so
/// a receiver typed from a `@types/*` package is matched against the list the
/// detection actually produced. Nothing else is rewritten.
fn normalize_declaring_package(package: &str) -> String {
    let Some(rest) = package.strip_prefix("@types/") else {
        return package.to_string();
    };
    match rest.split_once("__") {
        Some((scope, name)) => format!("@{scope}/{name}"),
        None => rest.to_string(),
    }
}

/// Replace the CONTENTS of string-literal types with nothing, keeping the
/// quotes, so `{ kind: "any" }` cannot false-positive the token scan.
fn strip_string_literal_contents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let mut escaped = false;
            for inner in chars.by_ref() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if inner == '\\' {
                    escaped = true;
                } else if inner == quote {
                    out.push(quote);
                    break;
                }
            }
        }
    }
    out
}

/// A v1 inferred type text is usable as a literal anchor when it carries an
/// actual shape: not the unknown/any placeholders the scrubbers leave, and
/// not container-decayed text (`any[]`, `Promise<any>`, `Record<string,
/// any>`, `{ metadata: any }`) — a rejected text falls back to the
/// locator-based infer anchor, whose result the capture self-check owns.
fn usable_inferred_text(text: &str) -> Option<&str> {
    let trimmed = text.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() || contains_disqualifying_top_type(trimmed) {
        None
    } else {
        Some(trimmed)
    }
}

/// True when an inference for an alias came back BLIND: tsc resolved the use
/// site to a bare `any`/`unknown` — no anchor symbol, no array depth, no
/// shape. This is not "the type is scalar"; it is "the compiler could not see
/// the type at all", the routine CI shape whenever a payload flowed through an
/// unresolved third-party import on a bare checkout (#349).
///
/// Deliberately narrower than [`contains_disqualifying_top_type`]: a partially
/// decayed shape (`{ ok: boolean; count: any }`) still witnesses the use
/// site's array-ness, so it is NOT blindness and must not demote anything.
/// Only the bare top types mean the inference saw nothing.
fn inference_was_blind(inf: &crate::services::type_sidecar::InferredType) -> bool {
    inf.primary_type_symbol.is_none()
        && inf.array_depth.is_none()
        && text_is_bare_top_type(&inf.type_string)
}

/// True when a printed type text carries no shape whatever: it IS a top type,
/// rather than a shape with one somewhere inside it. `{ ok: boolean; count:
/// any }` still describes a payload and is not this; `any` describes nothing.
///
/// Deliberately narrower than [`contains_disqualifying_top_type`], which is the
/// compat question ("could this read compatible against anything?"). This is
/// the publication question ("is there anything here to show a reader?").
pub(crate) fn text_is_bare_top_type(text: &str) -> bool {
    matches!(text.trim().trim_end_matches(';').trim(), "any" | "unknown")
}

/// Root `any_provenance` reasons with which the inferrer DECIDED a payload has
/// no contract, as opposed to failing to see one. `no_success_payload`: every
/// response the route's handler sends is an error or a redirect (carrick#1161).
/// `no_request_body`: the located request read is a validated non-body part
/// (carrick#1166), or a request config that sets no body member
/// (carrick-cloud#1366). `projected_value_only`: every read of the call's result
/// takes a member out of it, so the site states a part of a payload and not a
/// payload (carrick#1375).
const DECIDED_ABSTAIN_REASONS: &[&str] = &[
    "no_success_payload",
    "no_request_body",
    "projected_value_only",
];

/// True when an inference answered a bare top type because the inferrer read
/// the use site and decided nothing there is a contract.
///
/// That answer is final. The capture's own infer anchor re-runs the raw
/// locator without the inferrer's kind awareness, so for a redirect-only route
/// it would read the redirect location back in and publish `string` as the
/// body, which is the exact answer the inferrer just declined to give.
fn inference_decided_no_contract(inf: &crate::services::type_sidecar::InferredType) -> bool {
    text_is_bare_top_type(&inf.type_string)
        && inf
            .any_provenance
            .iter()
            .any(|p| p.path.is_empty() && DECIDED_ABSTAIN_REASONS.contains(&p.reason.as_str()))
}

/// Aliases whose deterministic inference ran and came back blind for EVERY
/// result it produced. A single sighted inference for the alias clears it: the
/// depth join (`apply_inferred_array_depth`) is first-anchor-carrying-wins, so
/// blindness is only meaningful when nothing else saw the use site.
fn blind_inference_aliases(
    inferred: &[crate::services::type_sidecar::InferredType],
) -> HashSet<&str> {
    let mut blind: HashSet<&str> = HashSet::new();
    let mut sighted: HashSet<&str> = HashSet::new();
    for inf in inferred {
        if inference_was_blind(inf) {
            blind.insert(inf.alias.as_str());
        } else {
            sighted.insert(inf.alias.as_str());
        }
    }
    blind.retain(|alias| !sighted.contains(alias));
    blind
}

/// Derive one capture anchor per alias from the collected v1 type requests.
///
/// Precedence mirrors the v1 bundle: an explicit symbol request wins over an
/// infer request for the same alias, which wins over an inline literal. The
/// alias strings are consumed as-is — this function never re-derives or
/// rewrites an alias, so the manifest join keys stay byte-identical.
///
/// For an infer-request alias, the v1 inference RESULT (when it produced a
/// real shape) rides a literal anchor at the structural_fallback tier: the
/// v1 inferrer is kind-aware (payload args, params, wrapper unwrapping via
/// the extraction config), which the capture-native locator is not yet — a
/// raw locator re-run resolves the wrong node for exactly those cases
/// (e.g. a `bus.emit(...)` boolean instead of its payload). The tier keeps
/// the legacy-text dependence measured and ratchetable; the locator-based
/// infer anchor remains the path for aliases v1 inference could not resolve.
///
/// `anchor_origin` mapping is pragmatic for WP3: symbol/literal anchors are
/// LLM-sourced (`llm-symbol`), infer-derived anchors are locator-driven
/// (`deterministic-infer`). Refining backfill attribution is follow-up work.
/// `manifest_aliases` is every alias the service's `type_manifest` declares —
/// the exact set the check phase will later import from this service's
/// surface. The manifest is derived from the mount graph (one alias per
/// operation x role x kind) while the anchors above are derived from the
/// collected type REQUESTS, and nothing reconciled the two: an operation
/// extraction could not type got a manifest alias, no anchor, no surface
/// export, and its pairs read *"the surface export is missing or renamed"* —
/// false, and it sends a reader hunting for a rename that does not exist
/// (carrick-cloud#1184). Every unrequested alias therefore ships as an
/// `unknown` placeholder, so the surface carries what the probe imports and
/// the check's IsUnknown gate states the truth: the type is unknown.
pub(crate) fn derive_capture_anchors(
    explicit: &[SymbolRequest],
    infer: &[InferRequestItem],
    inline_aliases: &[(String, String)],
    inferred: &[crate::services::type_sidecar::InferredType],
    manifest_aliases: &[String],
    repo_root: &str,
) -> Vec<CaptureAnchor> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut anchors: Vec<CaptureAnchor> = Vec::new();

    // First usable inferred text per alias (mirrors the enrich join's
    // first-wins `or_insert`).
    let mut inferred_text: HashMap<&str, &str> = HashMap::new();
    for inf in inferred {
        if let Some(text) = usable_inferred_text(&inf.type_string) {
            inferred_text.entry(inf.alias.as_str()).or_insert(text);
        }
    }
    let blind = blind_inference_aliases(inferred);
    let decided: HashSet<&str> = inferred
        .iter()
        .filter(|inf| inference_decided_no_contract(inf))
        .map(|inf| inf.alias.as_str())
        .collect();

    for request in explicit {
        let Some(alias) = request.alias.as_deref() else {
            // No alias means no manifest entry to join; nothing to capture.
            continue;
        };
        // An LLM symbol anchor is a BARE element identifier by schema contract
        // (`Order[]` -> `Order`), so the use-site's array-ness rides only on
        // `array_depth`, which `apply_inferred_array_depth` copies from the
        // deterministic inference for the same alias. When that inference came
        // back blind there is no depth to copy — and no way to tell "the type
        // is scalar" from "nothing was seen". Capturing the bare symbol anyway
        // publishes a CONFIDENT contract whose array-ness was guessed, which is
        // how a correct `Order[]` producer renders as `Order` and reads
        // incompatible against a correct `Order[]` consumer.
        //
        // Fail closed: emit no symbol anchor and leave the alias to its own
        // infer anchor below, which captures the decayed use site (`any`),
        // self-checks `decayed_internal`, and routes through the check's
        // IsAny gate to unverifiable. A depth the caller already knows (the
        // GraphQL SDL list marker, or a depth the join did land) is evidence
        // in its own right and keeps the anchor.
        if request.array_depth.is_none() && blind.contains(alias) {
            debug!(
                "v2 capture: alias {} has an explicit '{}' anchor but its inference \
                 resolved to a bare top type; skipping the symbol anchor so the \
                 array-ness is not guessed (pair verdicts unverifiable)",
                alias, request.symbol_name
            );
            continue;
        }
        if !seen.insert(alias.to_string()) {
            continue;
        }
        anchors.push(CaptureAnchor::Symbol {
            alias: alias.to_string(),
            symbol_name: request.symbol_name.clone(),
            source_file: repo_relative(&request.source_file, repo_root),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: request.array_depth.filter(|d| *d > 0),
        });
    }

    for request in infer {
        let Some(alias) = request.alias.as_deref() else {
            continue;
        };
        if !seen.insert(alias.to_string()) {
            continue;
        }
        // Kind-aware v1 inference result wins over a raw locator re-run.
        if let Some(text) = inferred_text.get(alias) {
            anchors.push(CaptureAnchor::Literal {
                alias: alias.to_string(),
                type_text: (*text).to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: Some(repo_relative(&request.file_path, repo_root)),
            });
            continue;
        }
        // The inferrer decided there is no contract here; a raw locator re-run
        // must not overrule it (`inference_decided_no_contract`).
        if decided.contains(alias) {
            anchors.push(CaptureAnchor::Literal {
                alias: alias.to_string(),
                type_text: "unknown".to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                // `unknown` names nothing, so no file needs to join the program.
                source_file: None,
            });
            continue;
        }
        // The capture locator prefers span, then expression text (from its
        // line), then first expression on the line — same precedence as the
        // v1 inferrer's locator inputs.
        let line_number = request
            .expression_line
            .filter(|l| *l > 0)
            .or(Some(request.line_number))
            .filter(|l| *l > 0);
        // #498: a `function_param` request names what a HANDLER RECEIVES, and
        // that is the whole locator a subscriber carries (its collector sends
        // no expression text on purpose). Dropping it here left the capture
        // with a bare line, whose locator resolves the enclosing registration
        // CALL — so every subscriber alias captured that call's return type
        // (`void`, a subscription handle) as its payload contract, self-checked
        // clean, and read incompatible against every correctly-typed publisher.
        // Gated on the kind: an expression-kind request must never carry one.
        let param_name = match request.infer_kind {
            InferKind::FunctionParam => request.param_name.clone(),
            _ => None,
        };
        anchors.push(CaptureAnchor::Infer {
            alias: alias.to_string(),
            source_file: repo_relative(&request.file_path, repo_root),
            anchor_origin: AnchorOrigin::DeterministicInfer,
            span_start: request.span_start,
            span_end: request.span_end,
            line_number,
            expression_text: request.expression_text.clone(),
            param_name,
        });
    }

    for (alias, type_text) in inline_aliases {
        if type_text.trim().is_empty() || !seen.insert(alias.clone()) {
            continue;
        }
        anchors.push(CaptureAnchor::Literal {
            alias: alias.clone(),
            type_text: type_text.clone(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            source_file: None,
        });
    }

    // Manifest aliases no request reached. Sorted so a stub's surface order is
    // a function of the manifest, not of hash iteration.
    let mut placeholders: Vec<&str> = manifest_aliases
        .iter()
        .map(String::as_str)
        .filter(|alias| !seen.contains(*alias))
        .collect();
    placeholders.sort_unstable();
    placeholders.dedup();
    for alias in placeholders {
        anchors.push(CaptureAnchor::Literal {
            alias: alias.to_string(),
            type_text: "unknown".to_string(),
            anchor_origin: AnchorOrigin::ManifestPlaceholder,
            // `unknown` names nothing, so no file needs to join the program.
            source_file: None,
        });
    }

    anchors
}

/// Run `capture_v2` for one service and read the stub package into the wire
/// artifact. Returns the on-disk stub dir (for the definitions re-point;
/// caller owns cleanup) alongside the artifact. `None` = capture degraded;
/// the service ships without a surface and its pairs verdict unverifiable.
///
/// When the first capture DEMOTES an alias (a `capture_failure_reason` on its
/// record — e.g. TS4023 skipped its file's declaration emit, so its surface
/// line was rewritten to `unknown`) and the scanner holds a usable literal
/// type text for that alias in `backfill_texts`, ONE backfill re-capture runs
/// with those anchors replaced by literal anchors (`anchor_origin:
/// anchor-backfill`). The re-captured artifact is adopted only when every
/// backfilled alias passes its self-check clean AND no sibling alias degraded
/// relative to the first run; otherwise the demoted (honest-unknown) artifact
/// is kept. The backfill is strictly post-hoc — it never enters the capture's
/// anchor arbitration, it only replaces an anchor whose capture already
/// demoted.
pub(crate) fn run_capture(
    sidecar: &TypeSidecar,
    repo_path: &str,
    service_id: &str,
    anchors: &[CaptureAnchor],
    backfill_texts: &HashMap<String, String>,
    tsconfig_path: Option<&str>,
) -> Option<(PathBuf, CaptureStubArtifact)> {
    if anchors.is_empty() {
        return None;
    }
    let repo_root = std::path::Path::new(repo_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(repo_path));

    let (stub_dir, artifact, records) =
        capture_once(sidecar, &repo_root, service_id, anchors, tsconfig_path)?;

    let Some((rerun_anchors, backfilled)) = backfill_anchors(anchors, &records, backfill_texts)
    else {
        return Some((stub_dir, artifact));
    };
    debug!(
        "v2 capture for {}: literal backfill attempt for {} demoted alias(es)",
        service_id,
        backfilled.len()
    );
    match capture_once(
        sidecar,
        &repo_root,
        service_id,
        &rerun_anchors,
        tsconfig_path,
    ) {
        Some((rerun_dir, rerun_artifact, rerun_records))
            if backfill_accepted(&backfilled, &records, &rerun_records) =>
        {
            debug!(
                "v2 capture for {}: backfill adopted ({} alias(es) re-anchored)",
                service_id,
                backfilled.len()
            );
            let _ = std::fs::remove_dir_all(&stub_dir);
            Some((rerun_dir, rerun_artifact))
        }
        Some((rerun_dir, _, _)) => {
            // Fail-closed: the backfill result failed its self-check or
            // degraded a sibling alias — keep the demoted/unknown surface
            // (honest unverifiable), never a backfill that didn't verify.
            debug!(
                "v2 capture for {}: backfill rejected by self-check; keeping demoted surface",
                service_id
            );
            let _ = std::fs::remove_dir_all(&rerun_dir);
            Some((stub_dir, artifact))
        }
        None => Some((stub_dir, artifact)),
    }
}

/// One `capture_v2` round-trip: run the capture into a fresh scratch dir and
/// read the stub package into the wire artifact, keeping the per-alias
/// records (the demotion signal the backfill decision needs).
fn capture_once(
    sidecar: &TypeSidecar,
    repo_root: &Path,
    service_id: &str,
    anchors: &[CaptureAnchor],
    tsconfig_path: Option<&str>,
) -> Option<(PathBuf, CaptureStubArtifact, Vec<CaptureAliasRecord>)> {
    let unique = format!(
        "carrick-capture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let out_dir = std::env::temp_dir().join(unique);

    let result = match sidecar.capture_v2(
        &repo_root.to_string_lossy(),
        service_id,
        anchors,
        &out_dir.to_string_lossy(),
        tsconfig_path,
    ) {
        Ok(result) => result,
        Err(e) => {
            warn!("v2 capture failed for {}: {}", service_id, e);
            let _ = std::fs::remove_dir_all(&out_dir);
            return None;
        }
    };

    debug!(
        "v2 capture for {}: {} alias(es), usable_rate {:.3}, {} emitted file(s), bare_checkout={}",
        service_id,
        result.fidelity.total_aliases,
        result.fidelity.usable_rate,
        result.emitted_files.len(),
        result.bare_checkout
    );

    let stub_dir = PathBuf::from(&result.stub_dir);
    match CaptureStubArtifact::from_stub_dir(
        &stub_dir,
        &result.package_name,
        &result.ts_version,
        result.bare_checkout,
    ) {
        Ok(artifact) => Some((stub_dir, artifact, result.aliases)),
        Err(e) => {
            warn!("failed to read capture stub for {}: {}", service_id, e);
            let _ = std::fs::remove_dir_all(&stub_dir);
            None
        }
    }
}

/// A scanner-held type text is usable as a BACKFILL literal anchor when it is
/// a real type EXPRESSION carrying an actual shape: the shared
/// disqualifying-top-type scan rejects any/unknown at any position (same rule
/// as the v1-inference literal tier), and declaration-shaped text is rejected
/// because the v1 bundle's fallback branches return whole declarations
/// (`interface X { ... }`, `export type X = ...`) that cannot sit on the
/// right-hand side of `export type A = ...;`.
fn usable_backfill_text(text: &str) -> Option<&str> {
    let trimmed = usable_inferred_text(text)?;
    const DECLARATION_PREFIXES: [&str; 8] = [
        "interface ",
        "class ",
        "enum ",
        "namespace ",
        "declare ",
        "export ",
        "abstract ",
        "function ",
    ];
    if DECLARATION_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        None
    } else {
        Some(trimmed)
    }
}

/// Alias -> literal type text the scanner can stand behind for a backfill
/// re-anchor, from the SAME v1 resolution results the enrich join consumes.
/// Precedence mirrors that join: the explicit bundle's structural expansion
/// wins over the inference result for the same alias; first usable text wins
/// within each source.
pub(crate) fn derive_backfill_texts(
    explicit_manifest: &[ManifestEntry],
    inferred: &[crate::services::type_sidecar::InferredType],
) -> HashMap<String, String> {
    let mut texts: HashMap<String, String> = HashMap::new();
    for entry in explicit_manifest {
        if let Some(text) = usable_backfill_text(&entry.type_string) {
            texts
                .entry(entry.alias.clone())
                .or_insert_with(|| text.to_string());
        }
    }
    for inf in inferred {
        if let Some(text) = usable_backfill_text(&inf.type_string) {
            texts
                .entry(inf.alias.clone())
                .or_insert_with(|| text.to_string());
        }
    }
    texts
}

/// Decide the backfill re-run's anchor set: every alias whose first-run
/// record shows a capture demotion (`capture_failure_reason` is present
/// exactly for demoted anchors) and for which a usable literal text exists
/// has its anchor replaced by a literal anchor at `anchor-backfill` origin.
/// Already-literal anchors are never re-anchored (the literal tier IS the
/// text tier — there is nothing better to fall back to). `None` = no
/// backfillable alias, skip the re-run entirely.
pub(crate) fn backfill_anchors(
    anchors: &[CaptureAnchor],
    records: &[CaptureAliasRecord],
    texts: &HashMap<String, String>,
) -> Option<(Vec<CaptureAnchor>, HashSet<String>)> {
    let mut backfilled: HashSet<String> = HashSet::new();
    for record in records {
        if record.capture_failure_reason.is_none() || record.anchor_kind == "literal" {
            continue;
        }
        if texts.contains_key(&record.alias) {
            backfilled.insert(record.alias.clone());
        }
    }
    if backfilled.is_empty() {
        return None;
    }
    let rerun = anchors
        .iter()
        .map(|anchor| {
            let alias = anchor.alias();
            if backfilled.contains(alias) {
                CaptureAnchor::Literal {
                    alias: alias.to_string(),
                    type_text: texts[alias].clone(),
                    anchor_origin: AnchorOrigin::AnchorBackfill,
                    source_file: anchor.source_file().map(str::to_string),
                }
            } else {
                anchor.clone()
            }
        })
        .collect();
    Some((rerun, backfilled))
}

/// Fail-closed acceptance for a backfill re-capture. Adopt only when:
///  - every backfilled alias re-captured CLEAN: no failure reason, self-check
///    `ok` (not merely allowlisted), no top type, no deep any/unknown; and
///  - no sibling alias degraded relative to the first run (usable stays
///    usable, no new failure reason, no new deep top type), so a backfill can
///    never smear the healthy part of the surface.
///
/// Anything else keeps the first (demoted, honest-unknown) artifact.
pub(crate) fn backfill_accepted(
    backfilled: &HashSet<String>,
    first: &[CaptureAliasRecord],
    rerun: &[CaptureAliasRecord],
) -> bool {
    let rerun_by_alias: HashMap<&str, &CaptureAliasRecord> =
        rerun.iter().map(|r| (r.alias.as_str(), r)).collect();
    let usable =
        |r: &CaptureAliasRecord| matches!(r.self_check.as_str(), "ok" | "allowlisted_external");

    for alias in backfilled {
        let Some(record) = rerun_by_alias.get(alias.as_str()) else {
            return false;
        };
        if record.capture_failure_reason.is_some()
            || record.self_check != "ok"
            || record.top_type_at_self_check
            || !record.any_provenance.is_empty()
        {
            return false;
        }
    }
    for record in first {
        if backfilled.contains(&record.alias) {
            continue;
        }
        let Some(rerun_record) = rerun_by_alias.get(record.alias.as_str()) else {
            return false;
        };
        if usable(record) && !usable(rerun_record) {
            return false;
        }
        if record.capture_failure_reason.is_none() && rerun_record.capture_failure_reason.is_some()
        {
            return false;
        }
        if record.any_provenance.is_empty() && !rerun_record.any_provenance.is_empty() {
            return false;
        }
        if !record.top_type_at_self_check && rerun_record.top_type_at_self_check {
            return false;
        }
    }
    true
}

// ===========================================================================
// Check time: pair building + check_v2 + outcome mapping
// ===========================================================================

/// Pseudo-method + join identity for a manifest entry, in exactly the format
/// `parse_producer_key` recovers from an edge's canonical producer key
/// (`("GET", "/orders/:id")`, `("SOCKET", "SERVER->CLIENT|event")`,
/// `("GRAPHQL", "query|field")`, `("PUBSUB", "topic")`).
fn join_identity(key: &OperationKey) -> Option<(String, String)> {
    match key {
        OperationKey::Http { method, path } => Some((method.to_uppercase(), path.clone())),
        OperationKey::Socket { event, direction } => Some((
            "SOCKET".to_string(),
            format!("{}|{}", direction.label(), event),
        )),
        OperationKey::Graphql { kind, field } => Some((
            "GRAPHQL".to_string(),
            format!("{}|{}", kind.as_str(), field),
        )),
        OperationKey::Pubsub { topic } => Some(("PUBSUB".to_string(), topic.clone())),
    }
}

/// Route-aware path normalization, ported from ts_check's `normalizePath`:
/// lowercase, single slashes, no trailing slash, every param syntax
/// (`:id`, `{id}`, `[id]`, `${expr}`) collapsed to `:param`.
fn normalize_match_path(input: &str) -> String {
    let mut normalized = input.to_lowercase();
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    normalized
        .split('/')
        .map(|seg| {
            let is_param = seg.starts_with(':')
                || (seg.starts_with('{') && seg.ends_with('}'))
                || (seg.starts_with('[') && seg.ends_with(']'))
                || seg.contains("${");
            if is_param {
                ":param".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Route-aware segment match, ported from ts_check's `pathsMatch`: equal
/// normalized paths, or segment-wise with `:param` as a wildcard.
fn paths_match(a: &str, b: &str) -> bool {
    let na = normalize_match_path(a);
    let nb = normalize_match_path(b);
    if na == nb {
        return true;
    }
    let sa: Vec<&str> = na.split('/').collect();
    let sb: Vec<&str> = nb.split('/').collect();
    if sa.len() != sb.len() {
        return false;
    }
    sa.iter()
        .zip(sb.iter())
        .all(|(x, y)| x == y || x.starts_with(':') || y.starts_with(':'))
}

/// Producer-specificity score, ported from ts_check's `calculateMatchScore`:
/// literal routes outrank parameterized ones for the same consumer.
fn match_score(producer_path: &str, consumer_path: &str) -> u8 {
    let np = normalize_match_path(producer_path);
    let nc = normalize_match_path(consumer_path);
    if np == nc {
        if producer_path == consumer_path {
            100
        } else {
            95
        }
    } else {
        90
    }
}

/// One built pair: the check spec plus everything the verdict join and the
/// findings projection need, so no information has to be re-parsed out of
/// labels after the verdict returns.
pub(crate) struct BuiltPair {
    pub spec: CheckPairSpec,
    /// Pseudo-method for the edge join (`GET` / `SOCKET` / `GRAPHQL` / `PUBSUB`).
    pub pseudo_method: String,
    /// Join identity: producer path (HTTP) or exact operation key tail.
    pub identity: String,
    /// Consumer call-site identity, `(file, line)` from the manifest entry.
    pub consumer_file: String,
    pub consumer_line: u32,
    pub type_kind: ManifestTypeKind,
    pub producer_alias: String,
    pub consumer_alias: String,
    pub producer_service: String,
    pub consumer_service: String,
    /// Set when the pair is unverifiable before any probe runs: a side has no
    /// v2 capture surface at all (older scan or degraded capture). The stale
    /// manifest `type_state` is NOT a pre-verdict trigger — check_v2 reads the
    /// real surface and judges resolved-ness itself.
    pub pre_verdict: Option<(VerdictBucket, String)>,
    /// Which side has no surface, when `pre_verdict` is set.
    pub pre_verdict_side: Option<VerdictSide>,
    /// The producer's response type, fully inlined, as the index publishes it
    /// (`expanded_definition`). The retype check states it at the consumer's
    /// call (carrick#1491).
    pub producer_expanded: Option<String>,
    /// The consumer's published type for the same kind. With the producer's,
    /// it says whether either side states a request body at all.
    pub consumer_expanded: Option<String>,
}

struct ServiceEntry<'a> {
    service_id: &'a str,
    has_surface: bool,
    entry: &'a TypeManifestEntry,
}

/// Build check pairs from every participating repo's manifest, cross-service
/// only (same-identity pairs are dropped by every matcher, #397/#410).
///
/// Port of the ts_check manifest-matcher pairing semantics:
/// - HTTP: method + route-aware path match + type_kind, keeping only the
///   most specific producer(s) per consumer.
/// - socket/graphql/pubsub: exact operation-key match + type_kind.
///
/// A side without a v2 capture surface produces a pair with a pre-set
/// unverifiable verdict instead of a probe ("peer scanned without a v2 surface
/// — re-scan"). The manifest `type_state` is NOT consulted for the pre-verdict:
/// it records the v1 (pre-capture) resolution, which leaves inline-literal
/// producer/consumer responses `Unknown` even when the v2 capture surface fully
/// resolved the alias. check_v2 reads the real surface and is the sole
/// authority — an alias baked to any/unknown (or absent) is caught by its own
/// gate. Gating the pre-verdict on the stale `type_state` dropped real,
/// resolvable, compatible edges to "not compared".
pub(crate) fn build_check_pairs(all_repo_data: &[CloudRepoData]) -> Vec<BuiltPair> {
    let mut producers: Vec<ServiceEntry> = Vec::new();
    let mut consumers: Vec<ServiceEntry> = Vec::new();

    for repo in all_repo_data {
        let service_id = repo
            .service_name
            .as_deref()
            .unwrap_or(repo.repo_name.as_str());
        let has_surface = repo
            .capture_stub
            .as_ref()
            .is_some_and(|s| s.artifact_version == CAPTURE_ARTIFACT_VERSION);
        let Some(entries) = repo.type_manifest.as_ref() else {
            continue;
        };
        for entry in entries {
            let target = match entry.role {
                ManifestRole::Producer => &mut producers,
                ManifestRole::Consumer => &mut consumers,
            };
            target.push(ServiceEntry {
                service_id,
                has_surface,
                entry,
            });
        }
    }

    let mut pairs: Vec<BuiltPair> = Vec::new();
    for consumer in &consumers {
        // Candidate producers, protocol-dispatched.
        let mut candidates: Vec<(&ServiceEntry, u8)> = Vec::new();
        for producer in &producers {
            if producer.service_id == consumer.service_id {
                continue;
            }
            if producer.entry.type_kind != consumer.entry.type_kind {
                continue;
            }
            match (&producer.entry.key, &consumer.entry.key) {
                (
                    OperationKey::Http {
                        method: pm,
                        path: pp,
                    },
                    OperationKey::Http {
                        method: cm,
                        path: cp,
                    },
                ) if pm.eq_ignore_ascii_case(cm) && paths_match(pp, cp) => {
                    candidates.push((producer, match_score(pp, cp)));
                }
                // Exact-key protocols: socket / graphql / pubsub.
                (
                    p @ (OperationKey::Socket { .. }
                    | OperationKey::Graphql { .. }
                    | OperationKey::Pubsub { .. }),
                    c,
                ) if p == c => {
                    candidates.push((producer, 100));
                }
                _ => {}
            }
        }
        if candidates.is_empty() {
            continue;
        }
        // HTTP specificity: keep only the best-scoring producer(s), mirroring
        // routing semantics (a literal route wins over :param).
        let best = candidates.iter().map(|(_, s)| *s).max().unwrap_or(0);
        for (producer, score) in candidates {
            if score != best {
                continue;
            }
            if let Some(pair) = build_pair(producer, consumer) {
                pairs.push(pair);
            }
        }
    }

    // Deterministic order regardless of repo download order.
    pairs.sort_by(|a, b| a.spec.pair_key.cmp(&b.spec.pair_key));
    pairs.dedup_by(|a, b| a.spec.pair_key == b.spec.pair_key);
    pairs
}

fn build_pair(producer: &ServiceEntry, consumer: &ServiceEntry) -> Option<BuiltPair> {
    let (pseudo_method, identity) = join_identity(&producer.entry.key)?;
    let protocol = match producer.entry.key {
        OperationKey::Http { .. } => ProbeProtocol::Http,
        OperationKey::Graphql { .. } => ProbeProtocol::Graphql,
        OperationKey::Socket { .. } => ProbeProtocol::Socket,
        OperationKey::Pubsub { .. } => ProbeProtocol::Pubsub,
    };
    // Socket/pubsub direction inverts regardless of manifest kind; the
    // sidecar's direction table keys on `both` for them.
    let type_kind = match (protocol, producer.entry.type_kind) {
        (ProbeProtocol::Socket | ProbeProtocol::Pubsub, _) => ProbeTypeKind::Both,
        (_, ManifestTypeKind::Request) => ProbeTypeKind::Request,
        (_, ManifestTypeKind::Response) => ProbeTypeKind::Response,
    };

    // Unique, deterministic pair key: both aliases embed the operation key,
    // role, kind, and (consumer-side) call site.
    let pair_key = format!(
        "{}/{}~{}/{}",
        producer.service_id,
        producer.entry.type_alias,
        consumer.service_id,
        consumer.entry.type_alias
    );

    // The ONLY pre-probe unverifiable is a side that literally has no v2
    // capture surface (older scan or capture degraded — re-scan it). The
    // manifest `type_state` is deliberately NOT consulted: it records the v1
    // (pre-capture) resolution, which leaves an inline-literal producer/
    // consumer response `Unknown` even when the v2 capture surface fully
    // resolved that alias (self-check ok). `check_v2` reads the actual surface
    // and is the sole authority on resolved-ness — an alias that baked to
    // `any`/`unknown` (or is absent from the surface) is caught by its own
    // IsAny/IsUnknown gate (`gate_caught_baked_any` / import error →
    // unverifiable) and can never read compatible. Gating on the stale
    // `type_state` instead dropped real, resolvable, mutually-compatible edges
    // to "not compared" (the order→notification verdict gap).
    let pre_verdict_side = if !producer.has_surface {
        Some(VerdictSide::Producer)
    } else if !consumer.has_surface {
        Some(VerdictSide::Consumer)
    } else {
        None
    };
    let pre_verdict = if !producer.has_surface {
        Some((
            VerdictBucket::Unverifiable,
            format!(
                "producer service '{}' has no v2 type surface (older scan or capture degraded) — re-scan it",
                producer.service_id
            ),
        ))
    } else if !consumer.has_surface {
        Some((
            VerdictBucket::Unverifiable,
            format!(
                "consumer service '{}' has no v2 type surface (older scan or capture degraded) — re-scan it",
                consumer.service_id
            ),
        ))
    } else {
        None
    };

    Some(BuiltPair {
        spec: CheckPairSpec {
            pair_key,
            protocol,
            type_kind,
            producer: CheckPairEndpoint {
                service_name: producer.service_id.to_string(),
                alias: producer.entry.type_alias.clone(),
            },
            consumer: CheckPairEndpoint {
                service_name: consumer.service_id.to_string(),
                alias: consumer.entry.type_alias.clone(),
            },
        },
        pseudo_method,
        identity,
        consumer_file: consumer.entry.file_path.clone(),
        consumer_line: consumer.entry.line_number,
        type_kind: producer.entry.type_kind,
        producer_alias: producer.entry.type_alias.clone(),
        consumer_alias: consumer.entry.type_alias.clone(),
        producer_service: producer.service_id.to_string(),
        consumer_service: consumer.service_id.to_string(),
        pre_verdict,
        pre_verdict_side,
        producer_expanded: producer.entry.expanded_definition.clone(),
        consumer_expanded: consumer.entry.expanded_definition.clone(),
    })
}

/// Materialize every artifact-carrying service's stub into `dest_root` and
/// return the check inputs. Stub dir names are keyed on the sanitized
/// service id (collisions suffixed) so two services of one monorepo can
/// never clobber each other's stub.
pub(crate) fn materialize_stubs(
    all_repo_data: &[CloudRepoData],
    dest_root: &Path,
) -> Vec<CheckStubInput> {
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut stubs = Vec::new();
    for repo in all_repo_data {
        let Some(artifact) = repo.capture_stub.as_ref() else {
            continue;
        };
        if artifact.artifact_version != CAPTURE_ARTIFACT_VERSION {
            continue;
        }
        let service_id = repo
            .service_name
            .as_deref()
            .unwrap_or(repo.repo_name.as_str());
        let base = service_id.replace(['/', '\\'], "_");
        let dir_name = match used.entry(base.clone()) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let n = e.get_mut();
                *n += 1;
                format!("{base}_{n}")
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(0);
                base
            }
        };
        let dest = dest_root.join(dir_name);
        if let Err(e) = artifact.materialize(&dest) {
            warn!("failed to materialize stub for {}: {}", service_id, e);
            continue;
        }
        stubs.push(CheckStubInput {
            service_name: service_id.to_string(),
            stub_dir: dest.to_string_lossy().into_owned(),
        });
    }
    stubs
}

/// Run the v2 check over the built pairs and produce the analyzer's pair
/// outcomes. Pairs with a pre-set verdict (no surface / unresolved side)
/// never reach the sidecar. When the check itself fails, every probing pair
/// degrades to unverifiable with the failure as the reason — never fatal to
/// the scan, and never read as compatible.
pub(crate) fn run_check(
    sidecar: &TypeSidecar,
    all_repo_data: &[CloudRepoData],
    local_consumers: &LocalConsumers,
) -> Vec<PairCheckOutcome> {
    let pairs = build_check_pairs(all_repo_data);
    if pairs.is_empty() {
        return Vec::new();
    }

    let mut outcomes: Vec<PairCheckOutcome> = Vec::new();
    // Pairs whose CONSUMER side left the verdict unresolved: the ones the
    // retype check can judge from the consumer's own code (carrick#1491).
    let mut consumer_blamed: HashSet<String> = HashSet::new();
    let mut probing: Vec<&BuiltPair> = Vec::new();
    for pair in &pairs {
        if pair.pre_verdict_side == Some(VerdictSide::Consumer) {
            consumer_blamed.insert(pair.spec.pair_key.clone());
        }
        if let Some((bucket, reason)) = &pair.pre_verdict {
            // A pre-verdicted pair never reached a probe, so nothing about it
            // was compared: not a fact, and the pre-verdict reason is why.
            outcomes.push(outcome_for(
                pair,
                *bucket,
                None,
                Some(reason.clone()),
                false,
                Some(reason.clone()),
                Vec::new(),
            ));
        } else {
            probing.push(pair);
        }
    }

    if !probing.is_empty() {
        let unique = format!(
            "carrick-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let workspace_parent = std::env::temp_dir().join(unique);
        let stubs = materialize_stubs(all_repo_data, &workspace_parent);
        let specs: Vec<CheckPairSpec> = probing.iter().map(|p| p.spec.clone()).collect();

        let check_result = if stubs.is_empty() {
            Err(crate::services::type_sidecar::SidecarError::CheckFailed(
                "no capture stubs available".to_string(),
            ))
        } else {
            sidecar.check_v2(&stubs, &specs)
        };

        match check_result {
            Ok(result) => {
                let by_key: BTreeMap<&str, &crate::services::type_sidecar::CheckVerdict> = result
                    .verdicts
                    .iter()
                    .map(|v| (v.pair_key.as_str(), v))
                    .collect();
                for pair in &probing {
                    match by_key.get(pair.spec.pair_key.as_str()) {
                        Some(verdict) => {
                            if consumer_to_blame(verdict) {
                                consumer_blamed.insert(pair.spec.pair_key.clone());
                            }
                            outcomes.push(outcome_for(
                                pair,
                                verdict.bucket,
                                verdict.gate.clone(),
                                verdict.diagnostic.clone(),
                                verdict.resolved,
                                verdict.unresolved_reason.clone(),
                                verdict.notes.clone(),
                            ))
                        }
                        None => outcomes.push(outcome_for(
                            pair,
                            VerdictBucket::Unverifiable,
                            None,
                            Some("the check returned no verdict for this pair".to_string()),
                            false,
                            Some("the check returned no verdict for this pair".to_string()),
                            Vec::new(),
                        )),
                    }
                }
                debug!(
                    "v2 check: {} pair(s) probed, {} pre-verdicted, ts {}",
                    probing.len(),
                    outcomes.len() - probing.len(),
                    result.ts_version
                );
            }
            Err(e) => {
                warn!(
                    "v2 check failed; all probing pairs degrade to unverifiable: {}",
                    e
                );
                let reason = format!("type check did not run: {}", e);
                for pair in &probing {
                    outcomes.push(outcome_for(
                        pair,
                        VerdictBucket::Unverifiable,
                        None,
                        Some(reason.clone()),
                        false,
                        Some(reason.clone()),
                        Vec::new(),
                    ));
                }
            }
        }

        let _ = std::fs::remove_dir_all(&workspace_parent);
    }

    retype_consumer_calls(
        sidecar,
        &pairs,
        &consumer_blamed,
        local_consumers,
        &mut outcomes,
    );

    // Deterministic order for every downstream consumer.
    outcomes.sort_by(|a, b| a.pair_key.cmp(&b.pair_key));
    log_unresolved_pairs(&outcomes, &pairs, local_consumers);
    outcomes
}

// ===========================================================================
// Check time: retype an unresolved consumer call (carrick#1491)
// ===========================================================================

/// The locator a consumer's `call_result` inference used for one response
/// alias. Carried from the scan to the check so the retype rewrites the call
/// the consumer's published type came from, not a guess at its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallLocator {
    /// Absolute path.
    pub file_path: String,
    pub line_number: u32,
    pub span_start: Option<u32>,
    pub span_end: Option<u32>,
    pub expression_text: Option<String>,
    pub expression_line: Option<u32>,
}

/// A service scanned in THIS run, whose sources are on disk: the only kind of
/// consumer whose file can be type-checked. A peer's blob carries no sources.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalConsumer {
    /// The root the sidecar is initialised at for this service.
    pub root: PathBuf,
    pub tsconfig: Option<String>,
    /// Consumer response alias -> the call its type was inferred from.
    pub calls: HashMap<String, CallLocator>,
}

/// Local consumers by service id (`service_name ?? repo_name`).
pub(crate) type LocalConsumers = HashMap<String, LocalConsumer>;

/// Filled while each local service is captured and read once, at the check.
/// The capture is the only place that holds the call locators, and it runs
/// several stack frames and one retry loop away from the check, so the run
/// hands them over here rather than through every frame in between.
static LOCAL_CONSUMERS: std::sync::Mutex<Option<LocalConsumers>> = std::sync::Mutex::new(None);

/// Record a local service's consumer calls for the retype check.
pub(crate) fn record_local_consumer(service_id: &str, consumer: LocalConsumer) {
    LOCAL_CONSUMERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(service_id.to_string(), consumer);
}

/// Take every recorded local consumer, leaving none.
pub(crate) fn take_local_consumers() -> LocalConsumers {
    LOCAL_CONSUMERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .unwrap_or_default()
}

/// The consumer response locators among a service's inference requests.
pub(crate) fn consumer_call_locators(infer: &[InferRequestItem]) -> HashMap<String, CallLocator> {
    let mut calls = HashMap::new();
    for request in infer {
        if request.infer_kind != InferKind::CallResult {
            continue;
        }
        let Some(alias) = request.alias.as_deref() else {
            continue;
        };
        // First locator wins, as the inference join does.
        calls
            .entry(alias.to_string())
            .or_insert_with(|| CallLocator {
                file_path: request.file_path.clone(),
                line_number: request.line_number,
                span_start: request.span_start,
                span_end: request.span_end,
                expression_text: request.expression_text.clone(),
                expression_line: request.expression_line,
            });
    }
    calls
}

/// Whether the CONSUMER's type is what left this verdict unresolved, in a way
/// a retype of its call could settle. A consumer that states no contract at
/// all (it reads no body, or sends a form body) has nothing to compare.
///
/// A proven mismatch is never a candidate: the retype may only lift a verdict
/// that compared nothing, never downgrade one that found a break.
fn consumer_to_blame(verdict: &crate::services::type_sidecar::CheckVerdict) -> bool {
    verdict.bucket != VerdictBucket::Incompatible
        && verdict.unresolved_side == Some(VerdictSide::Consumer)
        && !verdict
            .gate
            .as_deref()
            .is_some_and(|gate| gate.ends_with(":void") || gate.ends_with(":form"))
}

/// Said on every pair the retype decided.
const RETYPE_NOTE: &str = "judged by retyping the consumer's call with the producer's response \
     type and type-checking the consumer's own code";

/// The retype items to send, grouped by consumer service so each service's
/// program is built once. A pair qualifies when it is an HTTP response pair,
/// its CONSUMER left the verdict unresolved, the consumer was scanned in this
/// run (its file is on disk), and the producer published a response type with
/// no `any`/`unknown` in it.
fn retype_items<'a>(
    pairs: &'a [BuiltPair],
    consumer_blamed: &HashSet<String>,
    local_consumers: &LocalConsumers,
) -> BTreeMap<&'a str, Vec<RetypeItem>> {
    let mut by_service: BTreeMap<&str, Vec<RetypeItem>> = BTreeMap::new();
    for pair in pairs {
        if !consumer_blamed.contains(&pair.spec.pair_key)
            || pair.spec.protocol != ProbeProtocol::Http
            || pair.type_kind != ManifestTypeKind::Response
        {
            continue;
        }
        let Some(consumer) = local_consumers.get(&pair.consumer_service) else {
            continue;
        };
        let Some(call) = consumer.calls.get(&pair.consumer_alias) else {
            continue;
        };
        let Some(producer_type) = pair
            .producer_expanded
            .as_deref()
            .filter(|text| !contains_disqualifying_top_type(text) && !text.trim().is_empty())
        else {
            continue;
        };
        by_service
            .entry(pair.consumer_service.as_str())
            .or_default()
            .push(RetypeItem {
                item_id: pair.spec.pair_key.clone(),
                file_path: call.file_path.clone(),
                line_number: call.line_number,
                span_start: call.span_start,
                span_end: call.span_end,
                expression_text: call.expression_text.clone(),
                expression_line: call.expression_line,
                producer_type: producer_type.to_string(),
                wire: true,
            });
    }

    by_service
}

/// Judge every HTTP response pair whose CONSUMER left the verdict unresolved
/// by retyping the consumer's call with the producer's response type and
/// reading the consumer file's own type-check (carrick#1491).
///
/// Only a consumer scanned in this run can be judged (its file is on disk),
/// and only against a producer whose published type has no `any`/`unknown`
/// in it. A pair the retype decides becomes a fact either way: a mismatch
/// names each place the consumer uses what the producer does not return, and
/// an agreement is compatible. A pair it cannot decide keeps its verdict,
/// with the retype's reason added to why it is unresolved.
fn retype_consumer_calls(
    sidecar: &TypeSidecar,
    pairs: &[BuiltPair],
    consumer_blamed: &HashSet<String>,
    local_consumers: &LocalConsumers,
    outcomes: &mut [PairCheckOutcome],
) {
    let by_service = retype_items(pairs, consumer_blamed, local_consumers);
    for (service, items) in by_service {
        let consumer = &local_consumers[service];
        let answers = scope_to(sidecar, consumer).and_then(|()| sidecar.retype_check(&items));
        let answers: HashMap<String, RetypeOutcome> = match answers {
            Ok(answers) => answers
                .into_iter()
                .map(|answer| (answer.item_id.clone(), answer))
                .collect(),
            Err(e) => {
                warn!("Retyping {service}'s consumer calls failed: {e}");
                items
                    .iter()
                    .map(|item| {
                        (
                            item.item_id.clone(),
                            RetypeOutcome {
                                item_id: item.item_id.clone(),
                                outcome: RetypeVerdict::Abstain,
                                diagnostics: Vec::new(),
                                reason: Some(format!("the retype check did not run: {e}")),
                            },
                        )
                    })
                    .collect()
            }
        };
        for outcome in outcomes.iter_mut() {
            if let Some(answer) = answers.get(&outcome.pair_key) {
                apply_retype(outcome, answer);
            }
        }
    }
}

/// Point the sidecar's project at the consumer service, unless it already is.
fn scope_to(
    sidecar: &TypeSidecar,
    consumer: &LocalConsumer,
) -> Result<(), crate::services::type_sidecar::SidecarError> {
    if sidecar.is_scoped_to(&consumer.root, consumer.tsconfig.as_deref()) {
        return Ok(());
    }
    sidecar.start_init(&consumer.root, consumer.tsconfig.as_deref());
    sidecar.wait_ready(crate::services::type_sidecar::ready_budget())
}

/// Write one retype answer onto its pair's outcome. An outcome check_v2
/// already found incompatible keeps its verdict whatever the answer: the
/// retype only lifts what compared nothing.
fn apply_retype(outcome: &mut PairCheckOutcome, answer: &RetypeOutcome) {
    if outcome.bucket == VerdictBucket::Incompatible {
        return;
    }
    match answer.outcome {
        RetypeVerdict::Mismatch => {
            let places: Vec<String> = answer
                .diagnostics
                .iter()
                .map(|d| format!("{}:{}: {}", outcome.consumer_file, d.line, d.message))
                .collect();
            outcome.bucket = VerdictBucket::Incompatible;
            outcome.gate = Some("retype:consumer".to_string());
            outcome.diagnostic = Some(format!(
                "the consumer uses what the producer's response does not provide: {}",
                places.join("; ")
            ));
            outcome.resolved = true;
            outcome.unresolved_reason = None;
            outcome.notes.push(RETYPE_NOTE.to_string());
        }
        RetypeVerdict::Agrees => {
            outcome.bucket = VerdictBucket::Compatible;
            outcome.gate = None;
            outcome.diagnostic = None;
            outcome.resolved = true;
            outcome.unresolved_reason = None;
            outcome.notes.push(RETYPE_NOTE.to_string());
        }
        RetypeVerdict::Abstain => {
            let why = answer.reason.as_deref().unwrap_or("no reason given");
            outcome.unresolved_reason = Some(match outcome.unresolved_reason.take() {
                Some(reason) => {
                    format!("{reason}; retyping the consumer's call did not decide it: {why}")
                }
                None => format!("retyping the consumer's call did not decide it: {why}"),
            });
        }
    }
}

/// One log line per pair the check could not establish as a fact, with the
/// reason, so a CI run says why a pair is unverified without a second tool
/// (carrick#1491). Only the pairs this run is answerable for are logged.
fn log_unresolved_pairs(
    outcomes: &[PairCheckOutcome],
    pairs: &[BuiltPair],
    local_consumers: &LocalConsumers,
) {
    let worth: HashSet<&str> = pairs
        .iter()
        .filter(|pair| worth_logging(pair, local_consumers))
        .map(|pair| pair.spec.pair_key.as_str())
        .collect();
    for line in outcomes
        .iter()
        .filter(|o| worth.contains(o.pair_key.as_str()))
        .filter_map(unresolved_pair_line)
    {
        info!("{line}");
    }
}

/// Whether an unverified pair belongs in this run's log: it involves a
/// service scanned in this run (a pair between two peers is theirs to
/// report), and it is not the request half of an operation where neither side
/// publishes a request type, which states no body to compare rather than a
/// body nobody could read.
fn worth_logging(pair: &BuiltPair, local_consumers: &LocalConsumers) -> bool {
    let local = local_consumers.contains_key(&pair.producer_service)
        || local_consumers.contains_key(&pair.consumer_service);
    let no_body = pair.type_kind == ManifestTypeKind::Request
        && pair.producer_expanded.is_none()
        && pair.consumer_expanded.is_none();
    local && !no_body
}

/// The log line for one pair, or `None` when its verdict is a fact.
fn unresolved_pair_line(outcome: &PairCheckOutcome) -> Option<String> {
    if outcome.resolved {
        return None;
    }
    let kind = match outcome.type_kind {
        ManifestTypeKind::Request => "request",
        ManifestTypeKind::Response => "response",
    };
    Some(format!(
        "Types not verified: {} {} {} ({}:{} in {} against {}): {}",
        outcome.pseudo_method,
        outcome.identity,
        kind,
        outcome.consumer_file,
        outcome.consumer_line,
        outcome.consumer_service,
        outcome.producer_service,
        outcome
            .unresolved_reason
            .as_deref()
            .or(outcome.diagnostic.as_deref())
            .unwrap_or("no reason recorded")
    ))
}

fn outcome_for(
    pair: &BuiltPair,
    bucket: VerdictBucket,
    gate: Option<String>,
    diagnostic: Option<String>,
    resolved: bool,
    unresolved_reason: Option<String>,
    // `notes`: observations the check made beside the verdict (carrick#1341).
    // Empty for every outcome this module SYNTHESISES — a pre-verdicted pair,
    // a pair the check returned nothing for, a run that did not happen —
    // since none of those compared anything to observe. Only a real
    // `CheckVerdict.notes` is ever non-empty here.
    notes: Vec<String>,
) -> PairCheckOutcome {
    PairCheckOutcome {
        pair_key: pair.spec.pair_key.clone(),
        pseudo_method: pair.pseudo_method.clone(),
        identity: pair.identity.clone(),
        consumer_file: pair.consumer_file.clone(),
        consumer_line: pair.consumer_line,
        type_kind: pair.type_kind,
        bucket,
        gate,
        diagnostic,
        producer_alias: pair.producer_alias.clone(),
        consumer_alias: pair.consumer_alias.clone(),
        producer_service: pair.producer_service.clone(),
        consumer_service: pair.consumer_service.clone(),
        resolved,
        unresolved_reason,
        notes,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::{ManifestTypeState, TypeEvidence};
    use crate::services::type_sidecar::InferKind;
    use crate::type_manifest::{build_manifest_type_alias, build_manifest_type_alias_with_site_id};
    use serial_test::serial;

    // -----------------------------------------------------------------
    // carrick#695: receiver classification
    // -----------------------------------------------------------------

    fn lists() -> (Vec<String>, Vec<String>) {
        (
            vec!["server-fw".to_string(), "Express".to_string()],
            vec!["http-fetcher".to_string(), "@scope/fetcher".to_string()],
        )
    }

    #[test]
    fn receiver_declared_by_a_detected_framework_is_a_server() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver("Server", Some("server-fw"), &frameworks, &fetchers),
            Some(ReceiverRole::Server)
        );
    }

    #[test]
    fn receiver_declared_by_a_detected_fetcher_is_a_client() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver("HttpClient", Some("http-fetcher"), &frameworks, &fetchers),
            Some(ReceiverRole::Client)
        );
        assert_eq!(
            classify_receiver("Fetcher", Some("@scope/fetcher"), &frameworks, &fetchers),
            Some(ReceiverRole::Client)
        );
    }

    #[test]
    fn detection_list_matching_ignores_case() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver("Application", Some("express"), &frameworks, &fetchers),
            Some(ReceiverRole::Server)
        );
    }

    #[test]
    fn a_definitely_typed_package_is_matched_as_the_package_it_types() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver(
                "Application",
                Some("@types/express"),
                &frameworks,
                &fetchers
            ),
            Some(ReceiverRole::Server)
        );
        let scoped = (vec!["@hapi/hapi".to_string()], Vec::<String>::new());
        assert_eq!(
            classify_receiver("Server", Some("@types/hapi__hapi"), &scoped.0, &scoped.1),
            Some(ReceiverRole::Server)
        );
    }

    #[test]
    fn an_unresolved_receiver_states_no_role() {
        let (frameworks, fetchers) = lists();
        // The bare-checkout shape: every dependency-typed receiver is `any`.
        assert_eq!(
            classify_receiver("any", Some("server-fw"), &frameworks, &fetchers),
            None
        );
        assert_eq!(
            classify_receiver("unknown", None, &frameworks, &fetchers),
            None
        );
    }

    #[test]
    fn a_workspace_declared_receiver_stays_the_models() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver("{ get(path: string): void; }", None, &frameworks, &fetchers),
            None
        );
    }

    #[test]
    fn a_package_in_both_lists_is_ambiguous_and_states_nothing() {
        let both = vec!["full-stack".to_string()];
        assert_eq!(
            classify_receiver("Thing", Some("full-stack"), &both, &both),
            None
        );
    }

    #[test]
    fn a_package_in_neither_list_states_nothing() {
        let (frameworks, fetchers) = lists();
        assert_eq!(
            classify_receiver("Redis", Some("ioredis"), &frameworks, &fetchers),
            None
        );
    }

    fn entry(
        key: OperationKey,
        role: ManifestRole,
        type_kind: ManifestTypeKind,
        type_alias: &str,
        file_path: &str,
        line_number: u32,
        type_state: ManifestTypeState,
    ) -> TypeManifestEntry {
        TypeManifestEntry {
            key,
            role,
            type_kind,
            type_alias: type_alias.to_string(),
            file_path: file_path.to_string(),
            line_number,
            is_explicit: type_state == ManifestTypeState::Explicit,
            type_state,
            evidence: TypeEvidence {
                file_path: file_path.to_string(),
                span_start: None,
                span_end: None,
                line_number,
                infer_kind: InferKind::ResponseBody,
                is_explicit: false,
                type_state,
            },
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
        }
    }

    fn repo(
        repo_name: &str,
        service_name: Option<&str>,
        manifest: Vec<TypeManifestEntry>,
        capture_stub: Option<CaptureStubArtifact>,
    ) -> CloudRepoData {
        CloudRepoData {
            repo_name: repo_name.to_string(),
            service_name: service_name.map(str::to_string),
            endpoints: Vec::new(),
            calls: Vec::new(),
            mounts: Vec::new(),
            apps: HashMap::new(),
            imported_handlers: Vec::new(),
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test".to_string(),
            dirty: None,
            mount_graph: None,
            bundled_types: None,
            type_manifest: if manifest.is_empty() {
                None
            } else {
                Some(manifest)
            },
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            boundary: None,
            dispatch_tables: None,
        }
    }

    /// A peer that last uploaded under an older artifact schema is not judged
    /// against its stored stub: its declaration text can name a path that
    /// exists only on the machine that captured it (carrick#1174, carrick#1204)
    /// and its `carrick-manifest.json` carries none of the record fields the
    /// publish gate reads (carrick#1165, carrick#1164). It reads as no surface,
    /// with the re-scan reason, until it re-scans.
    #[test]
    fn a_peer_artifact_from_an_older_schema_has_no_surface() {
        let key = OperationKey::http("GET", "/orders");
        let stale = CaptureStubArtifact {
            artifact_version: CAPTURE_ARTIFACT_VERSION - 1,
            ..fake_artifact()
        };
        let producer = repo(
            "api",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "P",
                "src/routes.ts",
                3,
                ManifestTypeState::Explicit,
            )],
            Some(stale),
        );
        let consumer = repo(
            "web",
            None,
            vec![entry(
                key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "C",
                "src/client.ts",
                8,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );

        let pairs = build_check_pairs(&[producer, consumer]);
        assert_eq!(pairs.len(), 1);
        let (bucket, reason) = pairs[0].pre_verdict.as_ref().expect("pre-verdict");
        assert_eq!(*bucket, VerdictBucket::Unverifiable);
        assert!(reason.contains("no v2 type surface"), "{reason}");
    }

    fn fake_artifact() -> CaptureStubArtifact {
        CaptureStubArtifact {
            artifact_version: CAPTURE_ARTIFACT_VERSION,
            package_name: "@carrick/test".to_string(),
            ts_version: "5.8.3".to_string(),
            bare_checkout: true,
            files: BTreeMap::new(),
        }
    }

    // ---- retype check (carrick#1491) ---------------------------------------

    /// One producer/consumer HTTP pair on `POST /p`, both with a surface, the
    /// producer publishing `expanded` as its response.
    fn retype_pair(kind: ManifestTypeKind, expanded: Option<&str>) -> BuiltPair {
        let key = OperationKey::http("POST", "/p");
        let mut producer = entry(
            key.clone(),
            ManifestRole::Producer,
            kind,
            "P",
            "src/routes.ts",
            3,
            ManifestTypeState::Explicit,
        );
        producer.expanded_definition = expanded.map(str::to_string);
        let consumer = entry(
            key,
            ManifestRole::Consumer,
            kind,
            "C",
            "src/client.ts",
            8,
            ManifestTypeState::Unknown,
        );
        let mut pairs = build_check_pairs(&[
            repo("api", None, vec![producer], Some(fake_artifact())),
            repo("web", None, vec![consumer], Some(fake_artifact())),
        ]);
        assert_eq!(pairs.len(), 1);
        pairs.remove(0)
    }

    fn web_consumer() -> LocalConsumers {
        LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: PathBuf::from("/repo/web"),
                tsconfig: None,
                calls: HashMap::from([(
                    "C".to_string(),
                    CallLocator {
                        file_path: "/repo/web/src/client.ts".to_string(),
                        line_number: 8,
                        span_start: None,
                        span_end: None,
                        expression_text: Some("api.post('/p')".to_string()),
                        expression_line: Some(8),
                    },
                )]),
            },
        )])
    }

    #[test]
    fn retype_items_take_only_what_the_consumer_left_unresolved() {
        let local = web_consumer();
        let pair = retype_pair(ManifestTypeKind::Response, Some("{ id: string; }"));
        let blamed = HashSet::from([pair.spec.pair_key.clone()]);

        let pairs = [pair];
        let items = retype_items(&pairs, &blamed, &local);
        let web = &items["web"];
        assert_eq!(web.len(), 1);
        assert_eq!(web[0].item_id, pairs[0].spec.pair_key);
        assert_eq!(web[0].file_path, "/repo/web/src/client.ts");
        assert_eq!(web[0].expression_text.as_deref(), Some("api.post('/p')"));
        assert_eq!(web[0].producer_type, "{ id: string; }");
        assert!(web[0].wire, "an http response is judged in its wire form");

        // The producer, not the consumer, left it unresolved.
        assert!(retype_items(&pairs, &HashSet::new(), &local).is_empty());
        // The consumer is a peer: its file is not on this machine.
        assert!(retype_items(&pairs, &blamed, &LocalConsumers::new()).is_empty());

        // A producer type with a top type in it would let any read agree.
        let loose = [retype_pair(
            ManifestTypeKind::Response,
            Some("{ id: string; meta: any; }"),
        )];
        let blamed_loose = HashSet::from([loose[0].spec.pair_key.clone()]);
        assert!(retype_items(&loose, &blamed_loose, &local).is_empty());
        // No published producer type at all.
        let absent = [retype_pair(ManifestTypeKind::Response, None)];
        let blamed_absent = HashSet::from([absent[0].spec.pair_key.clone()]);
        assert!(retype_items(&absent, &blamed_absent, &local).is_empty());

        // A request body is what the consumer SENDS; retyping its call says
        // nothing about it.
        let request = [retype_pair(
            ManifestTypeKind::Request,
            Some("{ id: string; }"),
        )];
        let blamed_request = HashSet::from([request[0].spec.pair_key.clone()]);
        assert!(retype_items(&request, &blamed_request, &local).is_empty());
    }

    #[test]
    fn a_consumer_is_to_blame_only_for_a_type_a_retype_can_settle() {
        let verdict = |side: Option<VerdictSide>, gate: Option<&str>| {
            crate::services::type_sidecar::CheckVerdict {
                pair_id: "id".to_string(),
                pair_key: "key".to_string(),
                bucket: VerdictBucket::Unverifiable,
                gate: gate.map(str::to_string),
                diagnostic: None,
                codes: Vec::new(),
                resolved: false,
                unresolved_reason: Some("why".to_string()),
                unresolved_side: side,
                notes: Vec::new(),
            }
        };
        let consumer = Some(VerdictSide::Consumer);
        assert!(consumer_to_blame(&verdict(
            consumer,
            Some("consumer:unknown")
        )));
        assert!(consumer_to_blame(&verdict(
            consumer,
            Some("capture:consumer:any")
        )));
        // A deep finding on a compared pair carries no gate.
        assert!(consumer_to_blame(&verdict(consumer, None)));
        assert!(!consumer_to_blame(&verdict(
            Some(VerdictSide::Producer),
            Some("producer:any")
        )));
        assert!(!consumer_to_blame(&verdict(None, Some("assignment:other"))));
        // It reads no body: there is no use of a response to judge.
        assert!(!consumer_to_blame(&verdict(
            consumer,
            Some("consumer:void")
        )));
        assert!(!consumer_to_blame(&verdict(
            consumer,
            Some("consumer:form")
        )));
    }

    #[test]
    fn a_consumer_without_a_surface_is_the_side_to_blame() {
        let key = OperationKey::http("POST", "/p");
        let producer = entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            "P",
            "src/routes.ts",
            3,
            ManifestTypeState::Explicit,
        );
        let consumer = entry(
            key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "C",
            "src/client.ts",
            8,
            ManifestTypeState::Unknown,
        );
        let pairs = build_check_pairs(&[
            repo("api", None, vec![producer.clone()], Some(fake_artifact())),
            repo("web", None, vec![consumer.clone()], None),
        ]);
        assert_eq!(pairs[0].pre_verdict_side, Some(VerdictSide::Consumer));
        let pairs = build_check_pairs(&[
            repo("api", None, vec![producer], None),
            repo("web", None, vec![consumer], Some(fake_artifact())),
        ]);
        assert_eq!(pairs[0].pre_verdict_side, Some(VerdictSide::Producer));
    }

    #[test]
    fn a_retype_answer_lands_on_the_outcome() {
        let pair = retype_pair(ManifestTypeKind::Response, Some("{ y: number; }"));
        let unresolved = || {
            outcome_for(
                &pair,
                VerdictBucket::Unverifiable,
                Some("consumer:unknown".to_string()),
                Some("the consumer type resolved to 'unknown'".to_string()),
                false,
                Some("the consumer type is 'unknown'".to_string()),
                Vec::new(),
            )
        };
        let answer = |outcome: RetypeVerdict, reason: Option<&str>| RetypeOutcome {
            item_id: pair.spec.pair_key.clone(),
            outcome,
            diagnostics: vec![crate::services::type_sidecar::RetypeDiagnostic {
                line: 9,
                code: 2339,
                message: "Property 'x' does not exist on type '{ y: number; }'.".to_string(),
            }],
            reason: reason.map(str::to_string),
        };

        let mut mismatch = unresolved();
        apply_retype(&mut mismatch, &answer(RetypeVerdict::Mismatch, None));
        assert_eq!(mismatch.bucket, VerdictBucket::Incompatible);
        assert!(mismatch.resolved);
        assert_eq!(mismatch.unresolved_reason, None);
        assert_eq!(
            mismatch.diagnostic.as_deref(),
            Some(
                "the consumer uses what the producer's response does not provide: \
                 src/client.ts:9: Property 'x' does not exist on type '{ y: number; }'."
            )
        );
        assert_eq!(mismatch.notes, vec![RETYPE_NOTE.to_string()]);

        let mut agrees = unresolved();
        apply_retype(&mut agrees, &answer(RetypeVerdict::Agrees, None));
        assert_eq!(agrees.bucket, VerdictBucket::Compatible);
        assert!(agrees.resolved);
        assert_eq!((agrees.gate, agrees.diagnostic), (None, None));

        let mut abstain = unresolved();
        apply_retype(
            &mut abstain,
            &answer(
                RetypeVerdict::Abstain,
                Some("the consumer never reads the response"),
            ),
        );
        assert_eq!(abstain.bucket, VerdictBucket::Unverifiable);
        assert!(!abstain.resolved);
        assert_eq!(
            abstain.unresolved_reason.as_deref(),
            Some(
                "the consumer type is 'unknown'; retyping the consumer's call did not decide \
                 it: the consumer never reads the response"
            )
        );
    }

    #[test]
    fn only_pairs_this_run_answers_for_are_logged() {
        let local = web_consumer();
        let response = retype_pair(ManifestTypeKind::Response, Some("{ id: string; }"));
        assert!(worth_logging(&response, &local));
        // Two peers: nothing this run scanned.
        assert!(!worth_logging(&response, &LocalConsumers::new()));
        // A request half where neither side publishes a body type.
        let bodiless = retype_pair(ManifestTypeKind::Request, None);
        assert!(!worth_logging(&bodiless, &local));
        // One side does state a body: that half is worth a line.
        let with_body = retype_pair(ManifestTypeKind::Request, Some("{ id: string; }"));
        assert!(worth_logging(&with_body, &local));
    }

    #[test]
    fn a_retype_never_downgrades_a_proven_mismatch() {
        let pair = retype_pair(ManifestTypeKind::Response, Some("{ y: number; }"));
        let verdict = crate::services::type_sidecar::CheckVerdict {
            pair_id: "id".to_string(),
            pair_key: pair.spec.pair_key.clone(),
            bucket: VerdictBucket::Incompatible,
            gate: None,
            diagnostic: Some("Property 'y' is missing".to_string()),
            codes: vec![2741],
            resolved: false,
            unresolved_reason: Some("the consumer type carries 'any' at 'meta'".to_string()),
            unresolved_side: Some(VerdictSide::Consumer),
            notes: Vec::new(),
        };
        assert!(
            !consumer_to_blame(&verdict),
            "a check_v2 mismatch with a consumer deep-any is not a retype candidate"
        );
        let mut outcome = outcome_for(
            &pair,
            verdict.bucket,
            verdict.gate.clone(),
            verdict.diagnostic.clone(),
            verdict.resolved,
            verdict.unresolved_reason.clone(),
            Vec::new(),
        );
        apply_retype(
            &mut outcome,
            &RetypeOutcome {
                item_id: pair.spec.pair_key.clone(),
                outcome: RetypeVerdict::Agrees,
                diagnostics: Vec::new(),
                reason: None,
            },
        );
        assert_eq!(outcome.bucket, VerdictBucket::Incompatible);
        assert_eq!(
            outcome.diagnostic.as_deref(),
            Some("Property 'y' is missing")
        );
        assert!(!outcome.resolved);
    }

    /// The CI log states each unverified pair and why (carrick#1491): the
    /// smoke run listed a pair as not verifiable and printed no reason.
    #[test]
    fn an_unverified_pair_is_logged_with_its_reason() {
        let pair = retype_pair(ManifestTypeKind::Response, None);
        let outcome = outcome_for(
            &pair,
            VerdictBucket::GateCaughtBakedAny,
            Some("capture:consumer:any".to_string()),
            None,
            false,
            Some("the consumer type carries 'any' at '<1>'".to_string()),
            Vec::new(),
        );
        assert_eq!(
            unresolved_pair_line(&outcome).as_deref(),
            Some(
                "Types not verified: POST /p response (src/client.ts:8 in web against api): \
                 the consumer type carries 'any' at '<1>'"
            )
        );
        let fact = outcome_for(
            &pair,
            VerdictBucket::Compatible,
            None,
            None,
            true,
            None,
            Vec::new(),
        );
        assert_eq!(unresolved_pair_line(&fact), None);
    }

    #[test]
    fn consumer_call_locators_keep_the_first_call_result_per_alias() {
        let item = |kind: InferKind, alias: &str, line: u32| InferRequestItem {
            file_path: "/repo/web/src/client.ts".to_string(),
            line_number: line,
            span_start: Some(line * 10),
            span_end: Some(line * 10 + 5),
            expression_text: None,
            expression_line: None,
            infer_kind: kind,
            alias: Some(alias.to_string()),
            param_name: None,
        };
        let calls = consumer_call_locators(&[
            item(InferKind::RequestBody, "Req", 3),
            item(InferKind::CallResult, "C", 8),
            item(InferKind::CallResult, "C", 12),
        ]);
        assert_eq!(calls.len(), 1, "only call results are retyped: {calls:?}");
        assert_eq!(calls["C"].line_number, 8);
        assert_eq!(calls["C"].span_start, Some(80));
    }

    // ---- derive_capture_anchors -------------------------------------------

    /// Precedence per alias mirrors the v1 bundle: symbol wins over infer,
    /// infer wins over literal; aliases pass through byte-identical.
    #[test]
    fn derive_anchors_precedence_and_alias_passthrough() {
        let explicit = vec![SymbolRequest {
            symbol_name: "Order".to_string(),
            source_file: "src/types.ts".to_string(),
            alias: Some("Endpoint_a_Response".to_string()),
            array_depth: Some(1),
            payload_borrow_witness: false,
        }];
        let infer = vec![
            crate::services::type_sidecar::InferRequestItem {
                file_path: "/repo/src/handler.ts".to_string(),
                line_number: 7,
                span_start: Some(10),
                span_end: Some(20),
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::ResponseBody,
                alias: Some("Endpoint_a_Response".to_string()),
                param_name: None,
            },
            crate::services::type_sidecar::InferRequestItem {
                file_path: "/repo/src/handler.ts".to_string(),
                line_number: 9,
                span_start: None,
                span_end: None,
                expression_text: Some("payload".to_string()),
                expression_line: Some(9),
                infer_kind: InferKind::ResponseBody,
                alias: Some("Endpoint_b_Response".to_string()),
                param_name: None,
            },
        ];
        let inline = vec![
            ("Endpoint_b_Response".to_string(), "Widget".to_string()),
            (
                "Endpoint_c_Response".to_string(),
                "{ ok: boolean }".to_string(),
            ),
        ];

        let anchors = derive_capture_anchors(&explicit, &infer, &inline, &[], &[], "/repo");
        assert_eq!(anchors.len(), 3);

        match &anchors[0] {
            CaptureAnchor::Symbol {
                alias,
                array_depth,
                source_file,
                ..
            } => {
                assert_eq!(alias, "Endpoint_a_Response");
                assert_eq!(*array_depth, Some(1));
                assert_eq!(source_file, "src/types.ts");
            }
            other => panic!("expected symbol anchor, got {:?}", other),
        }
        match &anchors[1] {
            CaptureAnchor::Infer {
                alias, source_file, ..
            } => {
                assert_eq!(alias, "Endpoint_b_Response");
                // Absolute path under the repo root is relativized for the wire.
                assert_eq!(source_file, "src/handler.ts");
            }
            other => panic!("expected infer anchor, got {:?}", other),
        }
        match &anchors[2] {
            CaptureAnchor::Literal {
                alias, type_text, ..
            } => {
                assert_eq!(alias, "Endpoint_c_Response");
                assert_eq!(type_text, "{ ok: boolean }");
            }
            other => panic!("expected literal anchor, got {:?}", other),
        }
    }

    fn inferred(
        alias: &str,
        type_string: &str,
        primary_type_symbol: Option<&str>,
        array_depth: Option<u32>,
    ) -> crate::services::type_sidecar::InferredType {
        crate::services::type_sidecar::InferredType {
            alias: alias.to_string(),
            type_string: type_string.to_string(),
            is_explicit: false,
            source_location: crate::services::type_sidecar::SourceLocation {
                file_path: "/repo/src/handler.ts".to_string(),
                start_line: 7,
                end_line: 7,
                start_column: Some(0),
                end_column: Some(0),
            },
            infer_kind: InferKind::ResponseBody,
            primary_type_symbol: primary_type_symbol.map(str::to_string),
            array_depth,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
        }
    }

    fn order_explicit(alias: &str) -> SymbolRequest {
        SymbolRequest {
            symbol_name: "Order".to_string(),
            source_file: "src/types.ts".to_string(),
            alias: Some(alias.to_string()),
            // The join found no depth to copy — the LLM anchor is bare by
            // schema contract, so this is the state every unjoined alias is in.
            array_depth: None,
            payload_borrow_witness: false,
        }
    }

    fn response_body_infer(alias: &str) -> InferRequestItem {
        InferRequestItem {
            file_path: "/repo/src/handler.ts".to_string(),
            line_number: 7,
            span_start: Some(10),
            span_end: Some(20),
            expression_text: None,
            expression_line: None,
            infer_kind: InferKind::ResponseBody,
            alias: Some(alias.to_string()),
            param_name: None,
        }
    }

    /// A manifest alias no type request reached still has to reach the
    /// surface, or the check's probe import fails and the pair reads "the
    /// surface export is missing or renamed" — a false statement that sends a
    /// reader hunting for a rename (carrick-cloud#1184).
    ///
    /// Live repro: on `scanner-evals` the producing service's manifest carried
    /// two route aliases (`type_state: unknown`, `line_number: 1`) while its
    /// capture stub listed 12 aliases, none of them those two. Every one of
    /// the project's 52 judged probe rows failed on that missing import.
    #[test]
    fn an_unrequested_manifest_alias_ships_as_an_unknown_placeholder() {
        let explicit = vec![order_explicit("Endpoint_requested_Response")];
        let manifest = vec![
            "Endpoint_unrequested_Response".to_string(),
            "Endpoint_requested_Response".to_string(),
            "Endpoint_alsoMissing_Request".to_string(),
            // A manifest can name the same alias on several rows (one per
            // dispatch case on a body-dispatch route); the surface declares it
            // once.
            "Endpoint_alsoMissing_Request".to_string(),
        ];

        let anchors = derive_capture_anchors(&explicit, &[], &[], &[], &manifest, "/repo");

        assert_eq!(
            anchors.len(),
            3,
            "one anchor per distinct alias, requested or not: {anchors:?}"
        );
        assert!(
            matches!(&anchors[0], CaptureAnchor::Symbol { alias, .. }
                if alias == "Endpoint_requested_Response"),
            "a requested alias keeps its real anchor: {:?}",
            anchors[0]
        );
        let placeholders: Vec<(&str, &str)> = anchors[1..]
            .iter()
            .map(|anchor| match anchor {
                CaptureAnchor::Literal {
                    alias,
                    type_text,
                    anchor_origin,
                    source_file,
                } => {
                    assert_eq!(*anchor_origin, AnchorOrigin::ManifestPlaceholder);
                    assert_eq!(*source_file, None, "`unknown` names no file to load");
                    (alias.as_str(), type_text.as_str())
                }
                other => panic!("expected a placeholder literal, got {other:?}"),
            })
            .collect();
        assert_eq!(
            placeholders,
            vec![
                ("Endpoint_alsoMissing_Request", "unknown"),
                ("Endpoint_unrequested_Response", "unknown"),
            ],
            "sorted, so the surface order follows the manifest and not a hash walk"
        );
    }

    /// A blind inference must never let the LLM's bare element symbol ride as
    /// a confident contract.
    ///
    /// Live repro (carrick-demo, scanner v0.3.7): user-service's
    /// `GET /api/users/:id/orders` does `res.json(userOrders)` where
    /// `userOrders` came from `(await axios.get<Order[]>(...)).data.filter(...)`.
    /// CI scans a bare checkout (#349), so `axios` is unresolved and the whole
    /// expression decays to `any`: the `response_body` inference returns
    /// `type_string: "any"` with NO `primary_type_symbol` and NO `array_depth`
    /// (measured offline against the real source). `apply_inferred_array_depth`
    /// then has nothing to copy, the `Order` symbol anchor captures at depth 0,
    /// and the surface line is `import('./types/order').Order` — so the correct
    /// `Order[]` producer reads incompatible against the correct `Order[]`
    /// consumer and ships a CAUTION type_mismatch on every PR.
    ///
    /// The array-ness cannot be recovered here (nothing deterministic witnesses
    /// it), so the only honest outcome is to stop claiming it: no symbol anchor,
    /// the alias falls to its own infer anchor, which captures `any`,
    /// self-checks `decayed_internal`, and verdicts unverifiable.
    #[test]
    fn derive_anchors_drops_symbol_anchor_when_the_inference_was_blind() {
        let explicit = vec![order_explicit("Endpoint_blind_Response")];
        let infer = vec![response_body_infer("Endpoint_blind_Response")];
        let inferred_types = vec![inferred("Endpoint_blind_Response", "any", None, None)];

        let anchors = derive_capture_anchors(&explicit, &infer, &[], &inferred_types, &[], "/repo");

        assert_eq!(
            anchors.len(),
            1,
            "expected exactly the infer anchor, got {:?}",
            anchors
        );
        match &anchors[0] {
            CaptureAnchor::Infer { alias, .. } => {
                assert_eq!(alias, "Endpoint_blind_Response")
            }
            other => panic!(
                "a blind inference must demote the LLM symbol anchor to its \
                 locator-based infer anchor, got {:?}",
                other
            ),
        }
    }

    /// carrick#1161: a route whose handler only errors or redirects is answered
    /// `unknown` with `no_success_payload`. The capture must keep that answer
    /// rather than re-run the raw locator, which lands on the redirect location
    /// and prints `string` as the route's body. A plain blind inference, with
    /// no decision recorded, still falls to its infer anchor.
    #[test]
    fn derive_anchors_keeps_an_inferrer_decision_that_there_is_no_body() {
        let mut decided = inferred("Endpoint_redirect_Response", "unknown", None, None);
        decided.any_provenance = vec![crate::services::type_sidecar::TypeProvenance {
            path: String::new(),
            kind: "unknown".to_string(),
            reason: "no_success_payload".to_string(),
            detail: None,
        }];
        let blind = inferred("Endpoint_blind_Response", "unknown", None, None);
        let infer = vec![
            response_body_infer("Endpoint_redirect_Response"),
            response_body_infer("Endpoint_blind_Response"),
        ];

        let anchors = derive_capture_anchors(&[], &infer, &[], &[decided, blind], &[], "/repo");

        assert_eq!(anchors.len(), 2, "{anchors:?}");
        match &anchors[0] {
            CaptureAnchor::Literal {
                alias, type_text, ..
            } => {
                assert_eq!(alias, "Endpoint_redirect_Response");
                assert_eq!(type_text, "unknown");
            }
            other => panic!("a decided abstain must stay a literal unknown, got {other:?}"),
        }
        assert!(
            matches!(&anchors[1], CaptureAnchor::Infer { alias, .. } if alias == "Endpoint_blind_Response"),
            "a blind inference without a decision keeps its infer anchor, got {:?}",
            anchors[1]
        );
    }

    /// carrick#1375: one operation is one alias, and a consumer's call sites
    /// for it are several inferences. A site the inferrer decided states no
    /// contract (a hook reading members off a query result) must not speak for
    /// the alias while a sibling site (the fetcher, which parses the declared
    /// envelope) answered it — the decision keeps a raw locator re-run away,
    /// it does not outrank a payload another site stated. Order matters: the
    /// abstain is listed first, as the file-order request that produced the
    /// live false mismatch was.
    #[test]
    fn derive_anchors_prefer_a_sighted_sibling_over_a_decided_abstain() {
        let mut abstained = inferred("Endpoint_prefs_Response", "unknown", None, None);
        abstained.any_provenance = vec![crate::services::type_sidecar::TypeProvenance {
            path: String::new(),
            kind: "unknown".to_string(),
            reason: "projected_value_only".to_string(),
            detail: None,
        }];
        let sighted = inferred(
            "Endpoint_prefs_Response",
            "{ flags: { [key: string]: boolean; }; version: string; }",
            Some("PreferenceEnvelope"),
            None,
        );
        let infer = vec![response_body_infer("Endpoint_prefs_Response")];

        let anchors = derive_capture_anchors(&[], &infer, &[], &[abstained, sighted], &[], "/repo");

        assert_eq!(anchors.len(), 1, "{anchors:?}");
        match &anchors[0] {
            CaptureAnchor::Literal {
                alias, type_text, ..
            } => {
                assert_eq!(alias, "Endpoint_prefs_Response");
                assert_eq!(
                    type_text, "{ flags: { [key: string]: boolean; }; version: string; }",
                    "the site that stated a payload owns the alias"
                );
            }
            other => panic!("a sighted sibling must carry the alias, got {other:?}"),
        }
    }

    /// `unknown` is the scrubbers' failed-inference placeholder and means the
    /// same thing as `any` here: nothing was seen.
    #[test]
    fn derive_anchors_drops_symbol_anchor_on_blind_unknown() {
        let explicit = vec![order_explicit("Endpoint_blind_Response")];
        let infer = vec![response_body_infer("Endpoint_blind_Response")];
        let inferred_types = vec![inferred("Endpoint_blind_Response", "unknown", None, None)];

        let anchors = derive_capture_anchors(&explicit, &infer, &[], &inferred_types, &[], "/repo");
        assert!(
            matches!(anchors.as_slice(), [CaptureAnchor::Infer { .. }]),
            "got {:?}",
            anchors
        );
    }

    /// The guard is blindness-only, and every other state keeps the anchor —
    /// this is what bounds the recall cost of the demotion.
    #[test]
    fn derive_anchors_keeps_symbol_anchor_whenever_the_inference_saw_anything() {
        // (a) Sighted inference that resolved a real shape: the depth join
        //     already ran upstream (depth Some(1) here), anchor kept.
        let sighted = derive_capture_anchors(
            &[SymbolRequest {
                array_depth: Some(1),
                ..order_explicit("Endpoint_a_Response")
            }],
            &[response_body_infer("Endpoint_a_Response")],
            &[],
            &[inferred(
                "Endpoint_a_Response",
                "{ id: number; }[]",
                Some("Order"),
                Some(1),
            )],
            &[],
            "/repo",
        );
        assert!(
            matches!(
                sighted.as_slice(),
                [CaptureAnchor::Symbol {
                    array_depth: Some(1),
                    ..
                }]
            ),
            "got {:?}",
            sighted
        );

        // (b) PARTIAL decay: a shape with an `any` member still witnesses the
        //     use site's array-ness, so it is not blindness. Using
        //     `contains_disqualifying_top_type` here instead would demote it
        //     and widen the blast radius well past the defect.
        let partial = derive_capture_anchors(
            &[order_explicit("Endpoint_b_Response")],
            &[response_body_infer("Endpoint_b_Response")],
            &[],
            &[inferred(
                "Endpoint_b_Response",
                "{ ok: boolean; count: any; }",
                None,
                None,
            )],
            &[],
            "/repo",
        );
        assert!(
            matches!(partial.as_slice(), [CaptureAnchor::Symbol { .. }]),
            "partial decay is not blindness; got {:?}",
            partial
        );

        // (c) No inference at all for the alias (socket/pub-sub explicit-only
        //     anchors): the guard is structurally inert.
        let no_inference = derive_capture_anchors(
            &[order_explicit("Endpoint_c_Response")],
            &[],
            &[],
            &[],
            &[],
            "/repo",
        );
        assert!(
            matches!(no_inference.as_slice(), [CaptureAnchor::Symbol { .. }]),
            "got {:?}",
            no_inference
        );

        // (d) One blind result but another sighted one for the SAME alias:
        //     something saw the use site, so the anchor is kept.
        let mixed = derive_capture_anchors(
            &[order_explicit("Endpoint_d_Response")],
            &[response_body_infer("Endpoint_d_Response")],
            &[],
            &[
                inferred("Endpoint_d_Response", "any", None, None),
                inferred("Endpoint_d_Response", "Order[]", Some("Order"), Some(1)),
            ],
            &[],
            "/repo",
        );
        assert!(
            matches!(mixed.as_slice(), [CaptureAnchor::Symbol { .. }]),
            "got {:?}",
            mixed
        );

        // (e) A caller-supplied depth (the GraphQL SDL list marker, #248) is
        //     evidence in its own right and survives a blind inference.
        let sdl_depth = derive_capture_anchors(
            &[SymbolRequest {
                array_depth: Some(1),
                ..order_explicit("Endpoint_e_Response")
            }],
            &[response_body_infer("Endpoint_e_Response")],
            &[],
            &[inferred("Endpoint_e_Response", "any", None, None)],
            &[],
            "/repo",
        );
        assert!(
            matches!(
                sdl_depth.as_slice(),
                [CaptureAnchor::Symbol {
                    array_depth: Some(1),
                    ..
                }]
            ),
            "got {:?}",
            sdl_depth
        );
    }

    // ---- contains_disqualifying_top_type ----------------------------------

    /// The shared notion: any/unknown as a TYPE token at any position
    /// disqualifies (adversarial-review finding 2); property names, string
    /// literals, and ordinary identifiers containing the words do not.
    #[test]
    fn disqualifying_top_type_token_scan() {
        for text in [
            "any",
            "unknown",
            "any[]",
            "Array<any>",
            "Promise<any>",
            "Record<string, any>",
            "{ [k: string]: any }",
            "{ orderId: string; metadata: any }",
            "{ a: { b: unknown } }",
            "{ items: any[] }",
            "Promise<{ data: unknown }>",
            "(string | any)[]",
        ] {
            assert!(
                contains_disqualifying_top_type(text),
                "must disqualify: {text}"
            );
        }
        for text in [
            "{ ok: boolean }",
            "string[]",
            "Promise<{ a: string }>",
            "{ kind: \"any\" }",
            "{ kind: 'unknown' }",
            "{ any: string }",
            "{ any?: string }",
            "{ unknown: number }",
            "{ company: string }",
            "Anything",
            "{ unknownField: number; anyhow: string }",
        ] {
            assert!(
                !contains_disqualifying_top_type(text),
                "must NOT disqualify: {text}"
            );
        }
    }

    /// Container-decayed v1 inference text (`Promise<any>`, `any[]`, member
    /// any) must NOT ride a literal anchor: pre-fix it became a literal
    /// surface alias that probed clean and read compatible. Rejected text
    /// falls back to the locator-based infer anchor.
    #[test]
    fn derive_anchors_rejects_container_decayed_inferred_text() {
        let infer_item = |alias: &str| crate::services::type_sidecar::InferRequestItem {
            file_path: "src/bus.ts".to_string(),
            line_number: 4,
            span_start: None,
            span_end: None,
            expression_text: Some("payload".to_string()),
            expression_line: Some(4),
            infer_kind: InferKind::Expression,
            alias: Some(alias.to_string()),
            param_name: None,
        };
        let inferred_type = |alias: &str, text: &str| crate::services::type_sidecar::InferredType {
            alias: alias.to_string(),
            type_string: text.to_string(),
            is_explicit: false,
            source_location: crate::services::type_sidecar::SourceLocation {
                file_path: "src/bus.ts".to_string(),
                start_line: 4,
                end_line: 4,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::Expression,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
        };

        let infer = vec![
            infer_item("Pub_PromiseAny"),
            infer_item("Pub_ArrayAny"),
            infer_item("Pub_MemberAny"),
            infer_item("Pub_Clean"),
        ];
        let inferred = vec![
            inferred_type("Pub_PromiseAny", "Promise<any>"),
            inferred_type("Pub_ArrayAny", "any[]"),
            inferred_type("Pub_MemberAny", "{ orderId: string; metadata: any }"),
            inferred_type("Pub_Clean", "{ ok: boolean }"),
        ];

        let anchors = derive_capture_anchors(&[], &infer, &[], &inferred, &[], ".");
        assert_eq!(anchors.len(), 4);
        for (anchor, alias) in
            anchors
                .iter()
                .zip(["Pub_PromiseAny", "Pub_ArrayAny", "Pub_MemberAny"])
        {
            match anchor {
                CaptureAnchor::Infer { alias: a, .. } => assert_eq!(a, alias),
                other => {
                    panic!("{alias}: decayed text must fall back to an infer anchor, got {other:?}")
                }
            }
        }
        match &anchors[3] {
            CaptureAnchor::Literal {
                alias, type_text, ..
            } => {
                assert_eq!(alias, "Pub_Clean");
                assert_eq!(type_text, "{ ok: boolean }");
            }
            other => panic!("clean text must stay a literal anchor, got {other:?}"),
        }
    }

    /// #498: a subscriber's request is a `function_param` locator — a payload
    /// PARAMETER name, with no expression text (its collector sends none on
    /// purpose). That locator must reach the capture anchor. Dropping it left
    /// a bare line, whose capture locator resolves the enclosing registration
    /// CALL and captures that call's return type as the payload contract.
    ///
    /// An expression-kind request must never carry one: the gate is the kind,
    /// not the presence of the field.
    #[test]
    fn derive_anchors_carry_function_param_locator_for_subscribers() {
        let request = |alias: &str, kind: InferKind, param: Option<&str>| {
            crate::services::type_sidecar::InferRequestItem {
                file_path: "src/subscriber.ts".to_string(),
                line_number: 12,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: kind,
                alias: Some(alias.to_string()),
                param_name: param.map(str::to_string),
            }
        };

        let infer = vec![
            request("Sub_Producer", InferKind::FunctionParam, Some("msg")),
            request(
                "Sub_Destructured",
                InferKind::FunctionParam,
                Some("{ orderId, total }"),
            ),
            // Same field set on a non-param kind: must be dropped.
            request("Pub_Consumer", InferKind::Expression, Some("msg")),
        ];

        let anchors = derive_capture_anchors(&[], &infer, &[], &[], &[], ".");
        assert_eq!(anchors.len(), 3);
        let param_of = |index: usize| match &anchors[index] {
            CaptureAnchor::Infer {
                param_name,
                line_number,
                ..
            } => {
                assert_eq!(*line_number, Some(12));
                param_name.clone()
            }
            other => panic!("expected an infer anchor, got {other:?}"),
        };
        assert_eq!(param_of(0).as_deref(), Some("msg"));
        assert_eq!(param_of(1).as_deref(), Some("{ orderId, total }"));
        assert_eq!(param_of(2), None);
    }

    /// An infer-request alias whose v1 inference produced a real shape rides
    /// a LITERAL anchor carrying that text (the kind-aware inference result);
    /// a placeholder text (`unknown`) keeps the locator-based infer anchor.
    #[test]
    fn derive_anchors_prefers_v1_inferred_text_for_infer_aliases() {
        let infer_item = |alias: &str| crate::services::type_sidecar::InferRequestItem {
            file_path: "src/bus.ts".to_string(),
            line_number: 4,
            span_start: None,
            span_end: None,
            expression_text: Some("payload".to_string()),
            expression_line: Some(4),
            infer_kind: InferKind::Expression,
            alias: Some(alias.to_string()),
            param_name: None,
        };
        let inferred_type = |alias: &str, text: &str| crate::services::type_sidecar::InferredType {
            alias: alias.to_string(),
            type_string: text.to_string(),
            is_explicit: false,
            source_location: crate::services::type_sidecar::SourceLocation {
                file_path: "src/bus.ts".to_string(),
                start_line: 4,
                end_line: 4,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::Expression,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
        };

        let infer = vec![infer_item("Pub_Resolved"), infer_item("Pub_Unresolved")];
        let inferred = vec![
            inferred_type("Pub_Resolved", "{ time: string; item: string; }"),
            inferred_type("Pub_Unresolved", "unknown"),
        ];

        let anchors = derive_capture_anchors(&[], &infer, &[], &inferred, &[], ".");
        assert_eq!(anchors.len(), 2);
        match &anchors[0] {
            CaptureAnchor::Literal {
                alias,
                type_text,
                anchor_origin,
                source_file,
            } => {
                assert_eq!(alias, "Pub_Resolved");
                assert_eq!(type_text, "{ time: string; item: string; }");
                assert_eq!(*anchor_origin, AnchorOrigin::DeterministicInfer);
                // #1165: the file the text was printed from rides along, so
                // the capture's program declares the names the text prints.
                assert_eq!(source_file.as_deref(), Some("src/bus.ts"));
            }
            other => panic!(
                "expected literal anchor from inferred text, got {:?}",
                other
            ),
        }
        match &anchors[1] {
            CaptureAnchor::Infer { alias, .. } => assert_eq!(alias, "Pub_Unresolved"),
            other => panic!("expected locator infer anchor, got {:?}", other),
        }
    }

    // ---- literal backfill (demoted-at-capture re-anchor) ------------------

    fn record(
        alias: &str,
        anchor_kind: &str,
        self_check: &str,
        failure: Option<&str>,
    ) -> CaptureAliasRecord {
        CaptureAliasRecord {
            alias: alias.to_string(),
            anchor_kind: anchor_kind.to_string(),
            symbol_name: None,
            source_file: "src/routes.ts".to_string(),
            anchor_origin: "llm-symbol".to_string(),
            serialization: if failure.is_some() {
                "structural_fallback"
            } else {
                "emitted"
            }
            .to_string(),
            self_check: self_check.to_string(),
            self_check_detail: None,
            capture_failure_reason: failure.map(str::to_string),
            top_type_at_self_check: failure.is_some(),
            any_provenance: Vec::new(),
            dangling_specifiers: Vec::new(),
            undeclared_names: Vec::new(),
        }
    }

    fn symbol_anchor(alias: &str) -> CaptureAnchor {
        CaptureAnchor::Symbol {
            alias: alias.to_string(),
            symbol_name: "Notification".to_string(),
            source_file: "src/routes.ts".to_string(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: None,
        }
    }

    /// Only a DEMOTED alias (capture_failure_reason present) with an
    /// available literal text is re-anchored, as a literal at
    /// `anchor-backfill` origin; healthy siblings, demoted aliases without
    /// text, and already-literal anchors pass through untouched.
    #[test]
    fn backfill_anchors_replaces_only_demoted_with_text() {
        let anchors = vec![
            symbol_anchor("A_demoted"),
            symbol_anchor("B_healthy"),
            symbol_anchor("C_demoted_no_text"),
            CaptureAnchor::Literal {
                alias: "D_literal_demoted".to_string(),
                type_text: "{ ok: boolean }".to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: None,
            },
        ];
        let records = vec![
            record(
                "A_demoted",
                "symbol",
                "decayed_internal",
                Some("emit skipped"),
            ),
            record("B_healthy", "symbol", "ok", None),
            record(
                "C_demoted_no_text",
                "symbol",
                "decayed_internal",
                Some("emit skipped"),
            ),
            record(
                "D_literal_demoted",
                "literal",
                "decayed_internal",
                Some("bad text"),
            ),
        ];
        let mut texts = HashMap::new();
        texts.insert(
            "A_demoted".to_string(),
            "{ id: string; read: boolean; }".to_string(),
        );
        texts.insert(
            "B_healthy".to_string(),
            "{ never: string }".to_string(), // healthy alias: text must be ignored
        );
        texts.insert(
            "D_literal_demoted".to_string(),
            "{ ok: boolean }".to_string(), // literal anchors are never re-anchored
        );

        let (rerun, backfilled) =
            backfill_anchors(&anchors, &records, &texts).expect("one backfillable alias");
        assert_eq!(backfilled.len(), 1);
        assert!(backfilled.contains("A_demoted"));
        assert_eq!(rerun.len(), anchors.len());
        match &rerun[0] {
            CaptureAnchor::Literal {
                alias,
                type_text,
                anchor_origin,
                source_file,
            } => {
                assert_eq!(alias, "A_demoted");
                assert_eq!(type_text, "{ id: string; read: boolean; }");
                assert_eq!(*anchor_origin, AnchorOrigin::AnchorBackfill);
                assert_eq!(
                    source_file.as_deref(),
                    Some("src/routes.ts"),
                    "the backfill keeps the replaced anchor's file"
                );
            }
            other => panic!("demoted alias must become a backfill literal, got {other:?}"),
        }
        assert_eq!(rerun[1], anchors[1], "healthy anchor untouched");
        assert_eq!(rerun[2], anchors[2], "no-text demotion keeps its anchor");
        assert_eq!(rerun[3], anchors[3], "literal anchor never re-anchored");
    }

    /// No demoted alias with text -> no re-run at all.
    #[test]
    fn backfill_anchors_none_without_candidates() {
        let anchors = vec![symbol_anchor("A")];
        let healthy = vec![record("A", "symbol", "ok", None)];
        let mut texts = HashMap::new();
        texts.insert("A".to_string(), "{ ok: boolean }".to_string());
        assert!(backfill_anchors(&anchors, &healthy, &texts).is_none());

        let demoted = vec![record(
            "A",
            "symbol",
            "decayed_internal",
            Some("emit skipped"),
        )];
        assert!(backfill_anchors(&anchors, &demoted, &HashMap::new()).is_none());
    }

    /// Acceptance is fail-closed: adopt only a clean backfill self-check with
    /// no sibling degradation; any miss keeps the demoted artifact.
    #[test]
    fn backfill_accepted_rules() {
        let backfilled: HashSet<String> = ["A".to_string()].into();
        let first = vec![
            record("A", "symbol", "decayed_internal", Some("emit skipped")),
            record("B", "symbol", "ok", None),
        ];

        // Clean re-run: backfilled alias ok, sibling unchanged.
        let clean = vec![
            record("A", "literal", "ok", None),
            record("B", "symbol", "ok", None),
        ];
        assert!(backfill_accepted(&backfilled, &first, &clean));

        // Backfilled alias still demoted (e.g. its text dangled) -> reject.
        let still_demoted = vec![
            record("A", "literal", "decayed_internal", Some("dangling")),
            record("B", "symbol", "ok", None),
        ];
        assert!(!backfill_accepted(&backfilled, &first, &still_demoted));

        // Backfilled alias merely allowlisted (not a clean ok) -> reject.
        let allowlisted = vec![
            record("A", "literal", "allowlisted_external", None),
            record("B", "symbol", "ok", None),
        ];
        assert!(!backfill_accepted(&backfilled, &first, &allowlisted));

        // Backfilled alias resolves but carries a top type -> reject.
        let mut top_typed = vec![
            record("A", "literal", "ok", None),
            record("B", "symbol", "ok", None),
        ];
        top_typed[0].top_type_at_self_check = true;
        assert!(!backfill_accepted(&backfilled, &first, &top_typed));

        // Backfilled alias carries a DEEP any/unknown -> reject.
        let mut deep = vec![
            record("A", "literal", "ok", None),
            record("B", "symbol", "ok", None),
        ];
        deep[0].any_provenance = vec![crate::services::type_sidecar::TypeProvenance {
            path: "meta".to_string(),
            kind: "any".to_string(),
            reason: "declared".to_string(),
            detail: None,
        }];
        assert!(!backfill_accepted(&backfilled, &first, &deep));

        // A previously-usable sibling degrades in the re-run -> reject.
        let sibling_degraded = vec![
            record("A", "literal", "ok", None),
            record("B", "symbol", "decayed_internal", None),
        ];
        assert!(!backfill_accepted(&backfilled, &first, &sibling_degraded));

        // A sibling newly gains a root top type while staying "usable" -> reject
        // (Copilot review on #426: still contradicts "no sibling degraded").
        let mut sibling_top_typed = vec![
            record("A", "literal", "ok", None),
            record("B", "symbol", "ok", None),
        ];
        sibling_top_typed[1].top_type_at_self_check = true;
        assert!(!backfill_accepted(&backfilled, &first, &sibling_top_typed));

        // A sibling record vanishes from the re-run -> reject.
        let sibling_missing = vec![record("A", "literal", "ok", None)];
        assert!(!backfill_accepted(&backfilled, &first, &sibling_missing));

        // The backfilled record vanishes from the re-run -> reject.
        let backfilled_missing = vec![record("B", "symbol", "ok", None)];
        assert!(!backfill_accepted(&backfilled, &first, &backfilled_missing));
    }

    /// Text sourcing mirrors the enrich join (explicit bundle text wins over
    /// inference) and rejects everything the scanner cannot stand behind:
    /// any/unknown at any position, and declaration-shaped bundle fallbacks
    /// that are not type expressions.
    #[test]
    fn derive_backfill_texts_precedence_and_filters() {
        let manifest_entry = |alias: &str, text: &str| ManifestEntry {
            alias: alias.to_string(),
            original_name: "T".to_string(),
            source_file: "src/types.ts".to_string(),
            type_string: text.to_string(),
            is_explicit: true,
        };
        let inferred_type = |alias: &str, text: &str| crate::services::type_sidecar::InferredType {
            alias: alias.to_string(),
            type_string: text.to_string(),
            is_explicit: false,
            source_location: crate::services::type_sidecar::SourceLocation {
                file_path: "src/routes.ts".to_string(),
                start_line: 4,
                end_line: 4,
                start_column: None,
                end_column: None,
            },
            infer_kind: InferKind::ResponseBody,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
        };

        let explicit = vec![
            manifest_entry("A", "{ id: string; read: boolean; }"),
            manifest_entry("B", "interface Notification { id: string; }"), // declaration text
            manifest_entry("C", "{ metadata: any }"),                      // poisoned
        ];
        let inferred = vec![
            inferred_type("A", "{ id: string }"), // loses to the explicit text
            inferred_type("B", "{ id: string; }"),
            inferred_type("C", "unknown"),
            inferred_type("D", "{ ok: boolean; }"),
        ];

        let texts = derive_backfill_texts(&explicit, &inferred);
        assert_eq!(
            texts.get("A").map(String::as_str),
            Some("{ id: string; read: boolean; }"),
            "explicit bundle text wins"
        );
        assert_eq!(
            texts.get("B").map(String::as_str),
            Some("{ id: string; }"),
            "declaration-shaped explicit text is rejected; inference fills in"
        );
        assert!(!texts.contains_key("C"), "poisoned texts never backfill");
        assert_eq!(texts.get("D").map(String::as_str), Some("{ ok: boolean; }"));
    }

    // ---- build_check_pairs ------------------------------------------------

    /// HTTP pairing is method + route-aware path + type_kind, cross-service
    /// only, and a literal producer outranks a parameterized one for the same
    /// consumer (the ts_check specificity rule).
    #[test]
    fn build_pairs_http_specificity_and_cross_service_only() {
        let key_param = OperationKey::http("GET", "/users/:id");
        let key_literal = OperationKey::http("GET", "/users/me");
        let consumer_key = OperationKey::http("GET", "/users/me");

        let producer_repo = repo(
            "api",
            None,
            vec![
                entry(
                    key_param.clone(),
                    ManifestRole::Producer,
                    ManifestTypeKind::Response,
                    "P_param",
                    "src/routes.ts",
                    3,
                    ManifestTypeState::Explicit,
                ),
                entry(
                    key_literal.clone(),
                    ManifestRole::Producer,
                    ManifestTypeKind::Response,
                    "P_literal",
                    "src/routes.ts",
                    9,
                    ManifestTypeState::Explicit,
                ),
            ],
            Some(fake_artifact()),
        );
        let consumer_repo = repo(
            "web",
            None,
            vec![entry(
                consumer_key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "C_me",
                "src/client.ts",
                12,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );
        // A same-identity repo carrying both sides must produce no pair.
        let self_repo = repo(
            "web",
            None,
            vec![entry(
                key_literal.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "P_self",
                "src/self.ts",
                1,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );

        let pairs = build_check_pairs(&[producer_repo, consumer_repo, self_repo]);
        assert_eq!(pairs.len(), 1, "one pair: the most specific producer wins");
        let pair = &pairs[0];
        assert_eq!(pair.producer_alias, "P_literal");
        assert_eq!(pair.consumer_alias, "C_me");
        assert_eq!(pair.pseudo_method, "GET");
        assert_eq!(pair.identity, "/users/me");
        assert_eq!(pair.consumer_file, "src/client.ts");
        assert_eq!(pair.consumer_line, 12);
        assert!(pair.pre_verdict.is_none());
        assert_eq!(pair.spec.protocol, ProbeProtocol::Http);
        assert_eq!(pair.spec.type_kind, ProbeTypeKind::Response);
    }

    /// Exact-key protocols pair on equal operation keys; socket/pubsub pairs
    /// probe as `both` (the direction table inverts them), and the identity
    /// matches what `parse_producer_key` recovers from an edge.
    #[test]
    fn build_pairs_exact_key_protocols() {
        let topic = OperationKey::pubsub("order.placed");
        let producer_repo = repo(
            "worker",
            Some("billing"),
            vec![entry(
                topic.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "P_topic",
                "src/subscriber.ts",
                4,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );
        let consumer_repo = repo(
            "orders",
            Some("orders-engine"),
            vec![entry(
                topic.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "C_topic",
                "src/publisher.ts",
                21,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );

        let pairs = build_check_pairs(&[producer_repo, consumer_repo]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].pseudo_method, "PUBSUB");
        assert_eq!(pairs[0].identity, "order.placed");
        assert_eq!(pairs[0].spec.protocol, ProbeProtocol::Pubsub);
        assert_eq!(pairs[0].spec.type_kind, ProbeTypeKind::Both);
        assert_eq!(pairs[0].producer_service, "billing");
        assert_eq!(pairs[0].consumer_service, "orders-engine");
    }

    /// A missing capture surface — and ONLY a missing surface — pre-verdicts
    /// the pair unverifiable before any probe. A manifest `type_state ==
    /// Unknown` on a side that HAS a surface no longer pre-verdicts: the
    /// manifest state is the stale v1 (pre-capture) signal, so a resolvable
    /// alias would be dropped; check_v2 reads the actual surface and judges it
    /// (baked any/unknown is caught by its own IsAny/IsUnknown gate).
    #[test]
    fn build_pairs_pre_verdicts_only_surfaceless() {
        let key = OperationKey::http("GET", "/orders");
        let no_surface_producer = repo(
            "api",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "P",
                "src/routes.ts",
                3,
                ManifestTypeState::Explicit,
            )],
            None, // no capture stub: older scan or degraded capture
        );
        let consumer_ok = repo(
            "web",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "C",
                "src/client.ts",
                8,
                ManifestTypeState::Explicit,
            )],
            Some(fake_artifact()),
        );
        let pairs = build_check_pairs(&[no_surface_producer, consumer_ok.clone()]);
        assert_eq!(pairs.len(), 1);
        let (bucket, reason) = pairs[0].pre_verdict.as_ref().expect("pre-verdict");
        assert_eq!(*bucket, VerdictBucket::Unverifiable);
        assert!(reason.contains("no v2 type surface"), "{reason}");

        // Unknown type_state on a side WITH a surface: NO pre-verdict. The
        // stale manifest state must not short-circuit a probe — check_v2 reads
        // the real surface and is the sole authority on resolved-ness.
        let unresolved_producer = repo(
            "api",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "P",
                "src/routes.ts",
                3,
                ManifestTypeState::Unknown,
            )],
            Some(fake_artifact()),
        );
        let pairs = build_check_pairs(&[unresolved_producer, consumer_ok]);
        assert_eq!(pairs.len(), 1);
        assert!(
            pairs[0].pre_verdict.is_none(),
            "an Unknown-state side WITH a surface must probe, not pre-verdict: {:?}",
            pairs[0].pre_verdict
        );
    }

    // ---- end-to-end: capture_v2 -> check_v2 -> pair outcomes -> edge join --

    /// Full deterministic integration of the WP3 path against the corpus-2
    /// fixture pair (orders-engine's OrderPlaced.total is a Money object;
    /// billing-svc expects a bare number — the deliberate incompatibility):
    ///
    ///   1. capture_v2 both services through the Rust client,
    ///   2. artifacts ride CloudRepoData.capture_stub,
    ///   3. run_check builds the pair, materializes stubs, runs check_v2,
    ///   4. the incompatible verdict joins the CrossRepoMatch edge by
    ///      structured identity (pair-ID join, no label parsing), and
    ///   5. outcomes are stable across two independent check runs.
    ///
    /// The check path is deterministic given the anchors (no LLM anywhere);
    /// the pnpm install is local-only for these bare fixtures.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn corpus_capture_check_join_end_to_end() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let orders_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/orders-engine");
        let billing_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/billing-svc");
        assert!(orders_repo.exists(), "fixture missing: {orders_repo:?}");
        assert!(billing_repo.exists(), "fixture missing: {billing_repo:?}");

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&orders_repo, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        // Aliases exactly as the manifest builder derives them.
        let key = OperationKey::http("GET", "/orders/latest");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let consumer_call_id = crate::type_manifest::build_site_id(
            "src/billing-call.ts",
            5,
            &key,
            billing_repo.to_str().unwrap(),
        );
        let consumer_alias = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&consumer_call_id),
        );

        // Capture both services (symbol anchors on each side's OrderPlaced).
        let (orders_stub, orders_artifact) = run_capture(
            &sidecar,
            orders_repo.to_str().unwrap(),
            "orders-engine",
            &[CaptureAnchor::Symbol {
                alias: producer_alias.clone(),
                symbol_name: "OrderPlaced".to_string(),
                source_file: "src/types/order.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
            }],
            &HashMap::new(),
            None,
        )
        .expect("orders-engine capture");
        let (billing_stub, billing_artifact) = run_capture(
            &sidecar,
            billing_repo.to_str().unwrap(),
            "billing-svc",
            &[CaptureAnchor::Symbol {
                alias: consumer_alias.clone(),
                symbol_name: "OrderPlaced".to_string(),
                source_file: "src/types/billing.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
            }],
            &HashMap::new(),
            None,
        )
        .expect("billing-svc capture");
        let _ = std::fs::remove_dir_all(&orders_stub);
        let _ = std::fs::remove_dir_all(&billing_stub);

        let all_repo_data = vec![
            repo(
                "orders-engine",
                None,
                vec![entry(
                    key.clone(),
                    ManifestRole::Producer,
                    ManifestTypeKind::Response,
                    &producer_alias,
                    "src/routes.ts",
                    3,
                    ManifestTypeState::Explicit,
                )],
                Some(orders_artifact),
            ),
            repo(
                "billing-svc",
                None,
                vec![entry(
                    key.clone(),
                    ManifestRole::Consumer,
                    ManifestTypeKind::Response,
                    &consumer_alias,
                    "src/billing-call.ts",
                    5,
                    ManifestTypeState::Explicit,
                )],
                Some(billing_artifact),
            ),
        ];

        let outcomes = run_check(&sidecar, &all_repo_data, &LocalConsumers::new());
        assert_eq!(outcomes.len(), 1, "exactly one matched pair");
        let outcome = &outcomes[0];
        assert_eq!(
            outcome.bucket,
            VerdictBucket::Incompatible,
            "the corpus-2 Money-object vs number mismatch must verdict \
             incompatible; diagnostic: {:?}",
            outcome.diagnostic
        );
        let diagnostic = outcome.diagnostic.as_deref().unwrap_or("");
        assert!(
            !diagnostic.contains("/tmp/") && !diagnostic.contains("/private/"),
            "diagnostic leaked a scratch path: {diagnostic}"
        );

        // Pair-ID join onto the CrossRepoMatch edge — structured identity,
        // no label parsing anywhere.
        let mut edges = vec![crate::analyzer::CrossRepoMatch {
            producer_repo: "orders-engine".to_string(),
            producer_key: key.canonical(),
            consumer_repo: "billing-svc".to_string(),
            consumer_key: key.canonical(),
            consumer_location: Some("src/billing-call.ts:5:9".to_string()),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }];
        crate::analyzer::apply_pair_outcomes(&outcomes, &mut edges);
        assert_eq!(
            edges[0].type_compatible,
            Some(false),
            "the incompatible verdict must land on the edge"
        );
        assert!(edges[0].mismatch_reason.is_some());

        // Determinism: a second independent check run yields the same
        // outcomes (pair keys, buckets, diagnostics).
        let outcomes_again = run_check(&sidecar, &all_repo_data, &LocalConsumers::new());
        let flat = |v: &[PairCheckOutcome]| {
            v.iter()
                .map(|o| {
                    format!(
                        "{}|{:?}|{}",
                        o.pair_key,
                        o.bucket,
                        o.diagnostic.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            flat(&outcomes),
            flat(&outcomes_again),
            "check outcomes must be byte-stable across runs"
        );
    }

    /// carrick#1491, the two-case fixture the ticket requires, end to end
    /// against the real sidecar ($0, no model): a consumer reads
    /// `response.data.x` after (a) a typed call and (b) the same call with no
    /// type argument, on a synthetic client whose envelope carries an `any`
    /// beside its payload.
    ///
    /// Without the retype both pairs stay unverified, which is what a real
    /// scan reported. With it, both are flagged at the read against a
    /// producer returning `{ y }`, and neither against one returning `{ x }`.
    /// The sidecar is scoped to the PRODUCER service when the check runs, as
    /// it is after a monorepo scans its services in turn, so the pass has to
    /// re-scope it to the consumer.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn untyped_consumer_call_is_judged_by_retyping_it() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let fixture = manifest_dir.join("tests/fixtures/retype-http-client");
        let api_root = fixture.join("api").canonicalize().expect("api fixture");
        let web_root = fixture.join("web").canonicalize().expect("web fixture");

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&web_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("POST", "/checkout");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let consumer_alias = |line: u32| {
            let site = crate::type_manifest::build_site_id(
                "src/checkout.ts",
                line,
                &key,
                web_root.to_str().unwrap(),
            );
            build_manifest_type_alias_with_site_id(
                &key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                Some(&site),
            )
        };
        let (typed_alias, untyped_alias) = (consumer_alias(6), consumer_alias(11));

        // The consumer's inference requests as a scan builds them: the call's
        // text and line, as the model reports them.
        let file = web_root
            .join("src/checkout.ts")
            .to_string_lossy()
            .into_owned();
        let call = |line: u32, text: &str, alias: &str| InferRequestItem {
            file_path: file.clone(),
            line_number: line,
            span_start: None,
            span_end: None,
            expression_text: Some(text.to_string()),
            expression_line: Some(line),
            infer_kind: InferKind::CallResult,
            alias: Some(alias.to_string()),
            param_name: None,
        };
        let infer = vec![
            call(6, "api.post<{ x: number }>(\"/checkout\")", &typed_alias),
            call(11, "api.post(\"/checkout\")", &untyped_alias),
        ];
        let inferred = sidecar
            .infer_types(&infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        let consumer_anchors = derive_capture_anchors(
            &[],
            &infer,
            &[],
            &inferred,
            &[typed_alias.clone(), untyped_alias.clone()],
            web_root.to_str().unwrap(),
        );
        let (web_stub, web_artifact) = run_capture(
            &sidecar,
            web_root.to_str().unwrap(),
            "web",
            &consumer_anchors,
            &HashMap::new(),
            None,
        )
        .expect("web capture");
        let _ = std::fs::remove_dir_all(&web_stub);

        // The producer's response, published the way a scan publishes it: the
        // capture's expanded definition.
        let producer = |type_text: &str| {
            let (dir, artifact) = run_capture(
                &sidecar,
                api_root.to_str().unwrap(),
                "api",
                &[CaptureAnchor::Literal {
                    alias: producer_alias.clone(),
                    type_text: type_text.to_string(),
                    anchor_origin: AnchorOrigin::LlmSymbol,
                    source_file: None,
                }],
                &HashMap::new(),
                None,
            )
            .expect("api capture");
            let expanded = sidecar
                .resolve_definitions(dir.to_str().unwrap(), std::slice::from_ref(&producer_alias))
                .expect("definitions")
                .remove(0)
                .expanded;
            let _ = std::fs::remove_dir_all(&dir);
            let mut entry = entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                &producer_alias,
                "src/checkout.ts",
                1,
                ManifestTypeState::Explicit,
            );
            entry.expanded_definition = Some(expanded);
            vec![
                repo("api", None, vec![entry], Some(artifact)),
                repo(
                    "web",
                    None,
                    vec![
                        entry_for(&key, &typed_alias, 6),
                        entry_for(&key, &untyped_alias, 11),
                    ],
                    Some(web_artifact.clone()),
                ),
            ]
        };
        fn entry_for(key: &OperationKey, alias: &str, line: u32) -> TypeManifestEntry {
            entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                alias,
                "src/checkout.ts",
                line,
                ManifestTypeState::Unknown,
            )
        }
        let local = LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: web_root.clone(),
                tsconfig: None,
                calls: consumer_call_locators(&infer),
            },
        )]);
        let by_line = |outcomes: &[PairCheckOutcome]| -> BTreeMap<u32, PairCheckOutcome> {
            outcomes
                .iter()
                .map(|o| (o.consumer_line, o.clone()))
                .collect()
        };

        let returns_y = producer("{ y: number }");

        // Control: no retype. Neither pair is established, which is the miss.
        let control = by_line(&run_check(&sidecar, &returns_y, &LocalConsumers::new()));
        assert_eq!(control.len(), 2, "{control:#?}");
        for (line, outcome) in &control {
            assert!(
                !outcome.resolved && outcome.bucket != VerdictBucket::Incompatible,
                "line {line} is unverified without the retype: {outcome:#?}"
            );
        }
        // Why the TYPED call was unverified too: with no wrapper rule, its
        // inferred type is the client's whole envelope, whose request-data
        // parameter defaults to `any`, so it cannot anchor a literal and the
        // capture re-reads the envelope. An installed client stops at the
        // capture pre-gate on that member; this fixture's ambient client
        // decays whole. Either way it is the consumer's `any`.
        let typed_gate = control[&6].gate.as_deref().unwrap_or_default();
        assert!(typed_gate.ends_with("consumer:any"), "{:#?}", control[&6]);
        // The untyped call only ever reads members off its result, so the
        // inferrer abstains (carrick#1375) and the consumer is `unknown`.
        assert_eq!(control[&11].gate.as_deref(), Some("consumer:unknown"));

        // A monorepo scans its services in turn; the sidecar is left on the
        // last one, not on the consumer.
        sidecar.start_init(&api_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("re-init");

        let flagged = by_line(&run_check(&sidecar, &returns_y, &local));
        for line in [6, 11] {
            let outcome = &flagged[&line];
            assert_eq!(outcome.bucket, VerdictBucket::Incompatible, "{outcome:#?}");
            assert!(
                outcome.resolved,
                "a retyped verdict is a fact: {outcome:#?}"
            );
            assert_eq!(outcome.unresolved_reason, None);
            assert_eq!(outcome.gate.as_deref(), Some("retype:consumer"));
            let diagnostic = outcome.diagnostic.as_deref().unwrap_or_default();
            assert!(
                diagnostic.contains(&format!("src/checkout.ts:{}:", line + 1))
                    && diagnostic.contains("Property 'x' does not exist"),
                "the read of the missing field is named at its line: {diagnostic}"
            );
            assert!(outcome.notes.iter().any(|n| n == RETYPE_NOTE));
        }

        // A consumer whose own capture degraded has no surface at all, so the
        // pair never reaches a probe. Its file is still on disk, and the
        // retype still reads it.
        let mut no_surface = returns_y.clone();
        no_surface[1].capture_stub = None;
        let degraded = by_line(&run_check(&sidecar, &no_surface, &local));
        for line in [6, 11] {
            assert_eq!(
                degraded[&line].bucket,
                VerdictBucket::Incompatible,
                "{:#?}",
                degraded[&line]
            );
        }

        let returns_x = producer("{ x: number }");
        let agreed = by_line(&run_check(&sidecar, &returns_x, &local));
        for line in [6, 11] {
            let outcome = &agreed[&line];
            assert_eq!(outcome.bucket, VerdictBucket::Compatible, "{outcome:#?}");
            assert!(outcome.resolved, "{outcome:#?}");
            assert_eq!(outcome.diagnostic, None);
        }
    }

    /// carrick#1493: a consumer that calls `fetch` and reads the body with
    /// `res.json()` is judged through capture, check_v2 and the retype, end to
    /// end against the real sidecar ($0, no model).
    ///
    /// `fetch` takes no type argument and returns a typed `Response`, so the
    /// retype used to abstain on the call. The payload is the body read on
    /// its binding, and that read is what now carries the producer's type:
    /// a read of a field the producer does not return is flagged at its
    /// line, and one it does return is compatible.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn fetch_body_read_is_judged_by_retyping_it() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let api_root = manifest_dir
            .join("tests/fixtures/retype-http-client/api")
            .canonicalize()
            .expect("api fixture");
        let web_dir = tempfile::tempdir().unwrap();
        let web_root = web_dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(web_root.join("src")).unwrap();
        std::fs::write(
            web_root.join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true,"target":"es2022","module":"esnext","moduleResolution":"bundler","lib":["es2022","dom"],"skipLibCheck":true},"include":["src"]}"#,
        )
        .unwrap();
        // Line numbers are read off this text; keep the two in step.
        std::fs::write(
            web_root.join("src/checkout.ts"),
            "export async function loadCheckout(): Promise<number> {\n  \
             const res = await fetch(\"/checkout\");\n  \
             const body = await res.json();\n  \
             return body.x;\n\
             }\n",
        )
        .unwrap();
        let (call_line, read_line) = (2, 4);

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&web_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("POST", "/checkout");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let site = crate::type_manifest::build_site_id(
            "src/checkout.ts",
            call_line,
            &key,
            web_root.to_str().unwrap(),
        );
        let consumer_alias = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&site),
        );
        let infer = vec![InferRequestItem {
            file_path: web_root
                .join("src/checkout.ts")
                .to_string_lossy()
                .into_owned(),
            line_number: call_line,
            span_start: None,
            span_end: None,
            expression_text: Some("fetch(\"/checkout\")".to_string()),
            expression_line: Some(call_line),
            infer_kind: InferKind::CallResult,
            alias: Some(consumer_alias.clone()),
            param_name: None,
        }];
        let inferred = sidecar
            .infer_types(&infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        let consumer_anchors = derive_capture_anchors(
            &[],
            &infer,
            &[],
            &inferred,
            std::slice::from_ref(&consumer_alias),
            web_root.to_str().unwrap(),
        );
        let (web_stub, web_artifact) = run_capture(
            &sidecar,
            web_root.to_str().unwrap(),
            "web",
            &consumer_anchors,
            &HashMap::new(),
            None,
        )
        .expect("web capture");
        let _ = std::fs::remove_dir_all(&web_stub);

        let producer = |type_text: &str| {
            let (dir, artifact) = run_capture(
                &sidecar,
                api_root.to_str().unwrap(),
                "api",
                &[CaptureAnchor::Literal {
                    alias: producer_alias.clone(),
                    type_text: type_text.to_string(),
                    anchor_origin: AnchorOrigin::LlmSymbol,
                    source_file: None,
                }],
                &HashMap::new(),
                None,
            )
            .expect("api capture");
            let expanded = sidecar
                .resolve_definitions(dir.to_str().unwrap(), std::slice::from_ref(&producer_alias))
                .expect("definitions")
                .remove(0)
                .expanded;
            let _ = std::fs::remove_dir_all(&dir);
            let mut producer_entry = entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                &producer_alias,
                "src/checkout.ts",
                1,
                ManifestTypeState::Explicit,
            );
            producer_entry.expanded_definition = Some(expanded);
            vec![
                repo("api", None, vec![producer_entry], Some(artifact)),
                repo(
                    "web",
                    None,
                    vec![entry(
                        key.clone(),
                        ManifestRole::Consumer,
                        ManifestTypeKind::Response,
                        &consumer_alias,
                        "src/checkout.ts",
                        call_line,
                        ManifestTypeState::Unknown,
                    )],
                    Some(web_artifact.clone()),
                ),
            ]
        };
        let local = LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: web_root.clone(),
                tsconfig: None,
                calls: consumer_call_locators(&infer),
            },
        )]);
        let only = |outcomes: Vec<PairCheckOutcome>| -> PairCheckOutcome {
            assert_eq!(outcomes.len(), 1, "{outcomes:#?}");
            outcomes.into_iter().next().unwrap()
        };

        let returns_y = producer("{ y: number }");

        // Control: no retype. The body read is `any`, so nothing is compared.
        let control = only(run_check(&sidecar, &returns_y, &LocalConsumers::new()));
        assert!(
            !control.resolved && control.bucket != VerdictBucket::Incompatible,
            "unverified without the retype: {control:#?}"
        );

        let flagged = only(run_check(&sidecar, &returns_y, &local));
        assert_eq!(flagged.bucket, VerdictBucket::Incompatible, "{flagged:#?}");
        assert!(flagged.resolved, "{flagged:#?}");
        assert_eq!(flagged.gate.as_deref(), Some("retype:consumer"));
        let diagnostic = flagged.diagnostic.as_deref().unwrap_or_default();
        assert!(
            diagnostic.contains(&format!("src/checkout.ts:{read_line}:"))
                && diagnostic.contains("Property 'x' does not exist"),
            "the read of the missing field is named at its line: {diagnostic}"
        );

        let agreed = only(run_check(&sidecar, &producer("{ x: number }"), &local));
        assert_eq!(agreed.bucket, VerdictBucket::Compatible, "{agreed:#?}");
        assert!(agreed.resolved, "{agreed:#?}");
        assert_eq!(agreed.diagnostic, None);
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn explicit_config_survives_initial_capture_and_backfill() {
        let sidecar_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built");
            return;
        }
        let repo = tempfile::tempdir().unwrap();
        // Selecting this Deno config would fail; the explicit TS selection
        // must reach both stateless capture requests.
        std::fs::write(
            repo.path().join("deno.json"),
            r#"{"compilerOptions":{"lib":["deno.worker"]}}"#,
        )
        .unwrap();
        std::fs::write(repo.path().join("selected.json"),
            r#"{"compilerOptions":{"strict":true,"target":"ESNext","module":"ESNext","moduleResolution":"Bundler"},"include":["main.ts"]}"#).unwrap();
        std::fs::write(
            repo.path().join("main.ts"),
            "export interface Selected { id: string }",
        )
        .unwrap();
        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(repo.path(), Some("selected.json"));
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");
        let anchors = ["Selected", "Missing"].map(|name| CaptureAnchor::Symbol {
            alias: name.to_string(),
            symbol_name: name.to_string(),
            source_file: "main.ts".to_string(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: None,
        });
        let backfill = HashMap::from([("Missing".to_string(), "{ count: number }".to_string())]);
        let (dir, artifact) = run_capture(
            &sidecar,
            repo.path().to_str().unwrap(),
            "mixed",
            &anchors,
            &backfill,
            Some("selected.json"),
        )
        .expect("explicit config must survive both capture attempts");
        let surface = artifact.files.get("types/surface.d.ts").unwrap();
        assert!(
            surface.contains("count: number"),
            "backfill was not adopted: {surface}"
        );
        assert!(!surface.contains("unknown"), "capture degraded: {surface}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The corpus-2 notifications-svc 4th-edge shape, end-to-end against the
    /// real sidecar ($0, no LLM):
    ///
    /// The routes file's declaration emit is skipped (TS4023: `export
    /// default app` is unnameable through the hand-rolled fastify
    /// `export =` ambient stub), so the first capture demotes the http
    /// producer alias to `unknown` (f2700a9 partial-emit keep). With the
    /// scanner's v1 literal text available, ONE backfill re-capture
    /// re-anchors the alias as a literal at `anchor-backfill` origin, and
    /// the GET /notifications/:id pair lands a REAL verdict (compatible
    /// against web-dashboard's matching Notification). Without the text
    /// the demoted surface ships as-is and the pair stays honestly
    /// unverifiable — never compatible.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn demoted_alias_literal_backfill_end_to_end() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let notif_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/notifications-svc");
        let dash_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/web-dashboard");
        assert!(notif_repo.exists(), "fixture missing: {notif_repo:?}");
        assert!(dash_repo.exists(), "fixture missing: {dash_repo:?}");

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&notif_repo, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("GET", "/notifications/:id");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let consumer_call_id = crate::type_manifest::build_site_id(
            "lib/api.ts",
            30,
            &key,
            dash_repo.to_str().unwrap(),
        );
        let consumer_alias = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&consumer_call_id),
        );

        // The producer anchors mirror the real scan: the http alias's symbol
        // lives in the TS4023-poisoned routes file; a pub/sub sibling lives
        // in the healthy events file.
        let producer_anchors = [
            CaptureAnchor::Symbol {
                alias: producer_alias.clone(),
                symbol_name: "Notification".to_string(),
                source_file: "src/http/routes.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
            },
            CaptureAnchor::Symbol {
                alias: "Pub_OrderPlacedEvent".to_string(),
                symbol_name: "OrderPlacedEvent".to_string(),
                source_file: "src/types/events.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
            },
        ];
        // The scanner-side literal text for the operation (what v1 resolution
        // knows for this alias — see the fixture's expected.json).
        let mut backfill_texts = HashMap::new();
        backfill_texts.insert(
            producer_alias.clone(),
            "{ id: string; message: string; read: boolean; }".to_string(),
        );

        // ---- Backfill path: the demoted alias is re-anchored ----
        let (notif_stub, notif_artifact) = run_capture(
            &sidecar,
            notif_repo.to_str().unwrap(),
            "notifications-svc",
            &producer_anchors,
            &backfill_texts,
            None,
        )
        .expect("notifications-svc capture");
        let surface = notif_artifact
            .files
            .get("types/surface.d.ts")
            .expect("surface in artifact");
        assert!(
            surface.contains(&format!("export type {} = {{", producer_alias))
                && surface.contains("read: boolean"),
            "backfilled alias must carry the literal shape, got:\n{surface}"
        );
        assert!(
            !surface.contains(&format!("export type {} = unknown;", producer_alias)),
            "backfilled alias must not stay unknown:\n{surface}"
        );
        assert!(
            surface.contains("OrderPlacedEvent"),
            "healthy sibling keeps its emitted import reference:\n{surface}"
        );

        // ---- Control: no literal text -> the demoted surface ships as-is ----
        let (control_stub, control_artifact) = run_capture(
            &sidecar,
            notif_repo.to_str().unwrap(),
            "notifications-svc",
            &producer_anchors,
            &HashMap::new(),
            None,
        )
        .expect("control capture");
        let control_surface = control_artifact
            .files
            .get("types/surface.d.ts")
            .expect("surface in control artifact");
        assert!(
            control_surface.contains(&format!("export type {} = unknown;", producer_alias)),
            "without literal text the demotion must stand:\n{control_surface}"
        );

        // ---- Consumer capture (web-dashboard's matching Notification) ----
        let (dash_stub, dash_artifact) = run_capture(
            &sidecar,
            dash_repo.to_str().unwrap(),
            "web-dashboard",
            &[CaptureAnchor::Symbol {
                alias: consumer_alias.clone(),
                symbol_name: "Notification".to_string(),
                source_file: "lib/api.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
            }],
            &HashMap::new(),
            None,
        )
        .expect("web-dashboard capture");
        let _ = std::fs::remove_dir_all(&notif_stub);
        let _ = std::fs::remove_dir_all(&control_stub);
        let _ = std::fs::remove_dir_all(&dash_stub);

        let producer_entry = entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            &producer_alias,
            "src/http/routes.ts",
            18,
            ManifestTypeState::Explicit,
        );
        let consumer_repo_data = repo(
            "web-dashboard",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                &consumer_alias,
                "lib/api.ts",
                30,
                ManifestTypeState::Explicit,
            )],
            Some(dash_artifact),
        );

        // Backfilled producer: the pair lands a REAL verdict.
        let outcomes = run_check(
            &sidecar,
            &[
                repo(
                    "notifications-svc",
                    None,
                    vec![producer_entry.clone()],
                    Some(notif_artifact),
                ),
                consumer_repo_data.clone(),
            ],
            &LocalConsumers::new(),
        );
        assert_eq!(outcomes.len(), 1, "exactly one matched pair");
        assert_eq!(
            outcomes[0].bucket,
            VerdictBucket::Compatible,
            "the backfilled Notification shape must verdict compatible; \
             diagnostic: {:?}",
            outcomes[0].diagnostic
        );

        // Control producer: the demoted alias stays honestly unverifiable.
        let control_outcomes = run_check(
            &sidecar,
            &[
                repo(
                    "notifications-svc",
                    None,
                    vec![producer_entry],
                    Some(control_artifact),
                ),
                consumer_repo_data,
            ],
            &LocalConsumers::new(),
        );
        assert_eq!(control_outcomes.len(), 1);
        assert_eq!(
            control_outcomes[0].bucket,
            VerdictBucket::Unverifiable,
            "without a backfill the demotion must never read compatible; \
             diagnostic: {:?}",
            control_outcomes[0].diagnostic
        );
    }

    /// Regression for the order→notification verdict gap: a producer whose
    /// manifest `type_state` is `Unknown` (an inline-literal response v1 could
    /// not resolve — `primary_type_symbol` null) but whose v2 capture surface
    /// DID resolve the alias (self-check ok) must land a REAL verdict, not a
    /// pre-probe `Unverifiable`.
    ///
    /// Pre-fix, `build_pair` short-circuited any `type_state == Unknown` side
    /// to Unverifiable off the stale v1 signal, so `apply_pair_outcomes`
    /// collapsed the edge to `type_compatible: None` and the cloud read "not
    /// compared" — even though both sides had resolvable, mutually-compatible
    /// types. This test captures notifications-svc's inline-literal `GET
    /// /health` (`{ status: string }`) through an INFER anchor — v1 leaves it
    /// Unknown, the v2 locator resolves it — and pairs it with a matching
    /// consumer; the pair must verdict Compatible. The `type_state == Unknown`
    /// on the producer entry is the exact production state the fix must no
    /// longer gate on.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn unknown_state_producer_with_resolved_surface_verifies_end_to_end() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let notif_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/notifications-svc");
        let dash_repo = manifest_dir.join("tests/fixtures/xrepo-corpus-2/web-dashboard");
        assert!(notif_repo.exists(), "fixture missing: {notif_repo:?}");
        assert!(dash_repo.exists(), "fixture missing: {dash_repo:?}");

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&notif_repo, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("GET", "/health");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let consumer_call_id = crate::type_manifest::build_site_id(
            "lib/api.ts",
            40,
            &key,
            dash_repo.to_str().unwrap(),
        );
        let consumer_alias = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&consumer_call_id),
        );

        // Producer: the inline-literal `/health` return, captured via an INFER
        // anchor at the return statement (line 27). v1 cannot resolve an inline
        // object literal (it stays Unknown in the manifest); the v2 capture
        // locator resolves `{ status: string }`.
        let (notif_stub, notif_artifact) = run_capture(
            &sidecar,
            notif_repo.to_str().unwrap(),
            "notifications-svc",
            &[CaptureAnchor::Infer {
                alias: producer_alias.clone(),
                source_file: "src/http/routes.ts".to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                span_start: None,
                span_end: None,
                line_number: Some(27),
                expression_text: None,
                param_name: None,
            }],
            &HashMap::new(),
            None,
        )
        .expect("notifications-svc capture");
        let surface = notif_artifact
            .files
            .get("types/surface.d.ts")
            .expect("surface in artifact");
        assert!(
            surface.contains(&format!("export type {} =", producer_alias))
                && surface.contains("status"),
            "the inline-literal producer must RESOLVE in the surface, got:\n{surface}"
        );
        assert!(
            !surface.contains(&format!("export type {} = unknown", producer_alias))
                && !surface.contains(&format!("export type {} = any", producer_alias)),
            "the producer surface must not be a top-type placeholder:\n{surface}"
        );

        // Consumer: a matching `{ status: string }` expected type.
        let (dash_stub, dash_artifact) = run_capture(
            &sidecar,
            dash_repo.to_str().unwrap(),
            "web-dashboard",
            &[CaptureAnchor::Literal {
                alias: consumer_alias.clone(),
                type_text: "{ status: string }".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                source_file: None,
            }],
            &HashMap::new(),
            None,
        )
        .expect("web-dashboard capture");
        let _ = std::fs::remove_dir_all(&notif_stub);
        let _ = std::fs::remove_dir_all(&dash_stub);

        // The producer manifest entry carries `type_state = Unknown` — the exact
        // production state for an inline-literal producer response (v1 could not
        // resolve it; the v2 capture surface above DID). Pre-fix, this alone
        // pre-verdicted the pair Unverifiable before check_v2 ever ran.
        let producer_entry = entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            &producer_alias,
            "src/http/routes.ts",
            26,
            ManifestTypeState::Unknown,
        );
        let consumer_entry = entry(
            key.clone(),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            &consumer_alias,
            "lib/api.ts",
            40,
            ManifestTypeState::Explicit,
        );

        let outcomes = run_check(
            &sidecar,
            &[
                repo(
                    "notifications-svc",
                    None,
                    vec![producer_entry],
                    Some(notif_artifact),
                ),
                repo(
                    "web-dashboard",
                    None,
                    vec![consumer_entry],
                    Some(dash_artifact),
                ),
            ],
            &LocalConsumers::new(),
        );
        assert_eq!(outcomes.len(), 1, "exactly one matched pair");
        assert_eq!(
            outcomes[0].bucket,
            VerdictBucket::Compatible,
            "an Unknown-state producer whose v2 surface resolved must verdict \
             compatible — never pre-verdicted Unverifiable off the stale \
             type_state; diagnostic: {:?}",
            outcomes[0].diagnostic
        );
    }
}
