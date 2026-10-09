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
    AnchorOrigin, CaptureAliasRecord, CaptureAnchor, CaptureV2Result, CheckPairEndpoint,
    CheckPairSpec, CheckStubInput, InferKind, InferRequestItem, ManifestEntry, OperationProgress,
    ProbeProtocol, ProbeTypeKind, ProcessEnd, RetypeItem, RetypeOutcome, RetypeVerdict,
    SidecarError, SymbolRequest, VerdictBucket, VerdictSide,
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
    ["any", "unknown"]
        .into_iter()
        .any(|keyword| scrubbed_text_uses_keyword_as_type(&scrubbed, keyword))
}

/// [`contains_disqualifying_top_type`] for one of the two keywords, over text
/// whose string literals are already emptied.
fn scrubbed_text_uses_keyword_as_type(scrubbed: &str, keyword: &str) -> bool {
    let bytes = scrubbed.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
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

/// The capture alias that carries a response alias's UNWIDENED reading
/// (carrick#1516): the handler's return with no literal widened, captured
/// beside the published type so the definitions pass can expand it the same
/// way. Never a manifest alias of its own.
pub(crate) fn unwidened_alias(alias: &str) -> String {
    format!("{alias}_Unwidened")
}

/// The capture alias that carries one case of a response alias's modes
/// (carrick#2054): the union the handler sends when the incoming message's
/// field `read` holds `value`, or any value no other case names (`None`).
/// Captured beside the published type like [`unwidened_alias`], and never a
/// manifest alias of its own.
pub(crate) fn mode_alias(
    alias: &str,
    read: &crate::services::type_sidecar::MessageRead,
    value: Option<&str>,
) -> String {
    match value {
        Some(value) => format!(
            "{alias}_Mode{:016x}",
            crate::type_manifest::fnv1a_hash(&format!(
                "{}|{}|{}",
                read.location.as_str(),
                read.field,
                value
            ))
        ),
        None => format!("{alias}_ModeOther"),
    }
}

/// The response modes of each alias, taken from the same inference that
/// supplies its published text (the first usable one, as
/// [`derive_capture_anchors`] takes it), with each case's capture alias and
/// text (carrick#2054). A case whose text is unusable is left out, and a
/// union with modes but no usable case keeps its `reads`.
pub(crate) fn inferred_response_modes(
    inferred: &[crate::services::type_sidecar::InferredType],
) -> HashMap<&str, (crate::cloud_storage::ResponseModes, Vec<&str>)> {
    let mut first: HashSet<&str> = HashSet::new();
    let mut modes = HashMap::new();
    for inf in inferred {
        if usable_inferred_text(&inf.type_string).is_none() || !first.insert(inf.alias.as_str()) {
            continue;
        }
        let Some(read_modes) = inf.response_modes.as_ref() else {
            continue;
        };
        if read_modes.reads.is_empty() {
            continue;
        }
        let placed = match read_modes.reads.as_slice() {
            [read] if read.location != crate::services::type_sidecar::MessageSource::Unplaced => {
                Some(read)
            }
            _ => None,
        };
        let mut cases = Vec::new();
        let mut texts = Vec::new();
        for case in placed.map_or(&[][..], |_| read_modes.cases.as_slice()) {
            let (Some(read), Some(text)) = (placed, usable_inferred_text(&case.type_string)) else {
                continue;
            };
            cases.push(crate::cloud_storage::ResponseModeCase {
                value: case.value.clone(),
                alias: mode_alias(&inf.alias, read, case.value.as_deref()),
                expanded: None,
            });
            texts.push(text);
        }
        modes.insert(
            inf.alias.as_str(),
            (
                crate::cloud_storage::ResponseModes {
                    reads: read_modes.reads.clone(),
                    cases,
                },
                texts,
            ),
        );
    }
    modes
}

/// True when an inference for an alias READ NO SHAPE: tsc resolved the use
/// site to a bare `any`/`unknown` and peeled no array level off it. This is
/// not "the type is scalar"; it is "the compiler could not see the type at
/// all", the routine CI shape whenever a payload flowed through an unresolved
/// third-party import on a bare checkout (#349), and the shape of a body the
/// source itself reads untyped and casts later (carrick#1967).
///
/// Deliberately narrower than [`contains_disqualifying_top_type`]: a partially
/// decayed shape (`{ ok: boolean; count: any }`) still witnesses the use
/// site's array-ness, so it is NOT this and must not demote anything. Only the
/// bare top types mean the inference saw nothing.
///
/// A symbol the answer names beside the bare top type is not part of the
/// question. Whether that symbol witnesses an anchor is asked of the anchor's
/// own symbol, by [`Sightings::none_of`].
///
/// An array level counts only beside an element symbol. A `call_result`
/// reports the depth of the call's result even where its text is a decayed
/// read of something after it, and a depth with no element named says nothing
/// of the symbol this is asked about (carrick#1967).
fn inference_read_no_shape(inf: &crate::services::type_sidecar::InferredType) -> bool {
    let peeled_a_named_list = inf.primary_type_symbol.is_some() && inf.array_depth.is_some();
    !peeled_a_named_list && text_is_bare_top_type(&inf.type_string)
}

/// What the deterministic inferences saw at each alias, asked of one model
/// symbol at a time (carrick#1967).
///
/// A model symbol is the bare element by schema contract (`Order[]` ->
/// `Order`), so it is published, as an anchor or as the text a backfill
/// re-anchors with, only at an array depth a deterministic reader witnessed:
/// the inference for the same alias naming the same symbol, the body's stated
/// type, or a depth the caller knows (a schema's list marker).
struct Sightings<'a> {
    by_alias: HashMap<&'a str, Vec<&'a crate::services::type_sidecar::InferredType>>,
}

impl<'a> Sightings<'a> {
    fn of(inferred: &'a [crate::services::type_sidecar::InferredType]) -> Self {
        let mut by_alias: HashMap<&str, Vec<&crate::services::type_sidecar::InferredType>> =
            HashMap::new();
        for inf in inferred {
            by_alias.entry(inf.alias.as_str()).or_default().push(inf);
        }
        Self { by_alias }
    }

    /// True when inferences ran for `alias` and none of them saw `symbol`:
    /// each read a bare top type, and none names the symbol itself. A single
    /// sighted inference clears it, because the depth join
    /// (`apply_inferred_array_depth`) is first-anchor-carrying-wins.
    ///
    /// The symbol an answer names beside a bare top type is a sighting only
    /// when it IS the anchor's symbol (an alias declared `any`). Any other
    /// one is not: where the source reads a body untyped, the answer names
    /// the transport's response object, which says nothing of the body. An
    /// alias no inference ran for is not this either; nothing looked.
    fn none_of(&self, alias: &str, symbol: &str) -> bool {
        self.by_alias.get(alias).is_some_and(|answers| {
            answers.iter().all(|inf| {
                inference_read_no_shape(inf) && inf.primary_type_symbol.as_deref() != Some(symbol)
            })
        })
    }
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

/// How many findings the capture's deep walk lists per alias before it stops
/// listing (`MAX_DEEP_FINDINGS` in `src/sidecar/src/capture/deep-walk.ts`).
/// A record holding this many may have more positions it never named, so it
/// cannot show that every top type in its type is accounted for.
pub(crate) const CAPTURE_FINDINGS_CAP: usize = 32;

/// True when every top type in a capture answer is a member the SOURCE
/// declares `unknown` (carrick#1752): the answer is a typed contract with open
/// fields, not a type no layer could see.
///
/// [`contains_disqualifying_top_type`] cannot ask this. It reads text, and in
/// text a member the author typed `unknown` and one the pipeline wrote
/// `unknown` for an import that did not resolve (carrick#1377, carrick#1397)
/// are the same word. The capture's record tells them apart: its deep walk
/// names every position a top type sits at, with a cause. So the answer
/// qualifies only when all of these hold:
///
///  - the text carries no `any`. `any` is bidirectionally assignable, and the
///    walk does not read parameter positions, so the text is the one place a
///    parameter `any` shows;
///  - the record names at least one finding and fewer than the walk lists
///    before it stops ([`CAPTURE_FINDINGS_CAP`]), so every position the text
///    holds is accounted for. A root that is itself a top type has no members
///    to walk, so it names none;
///  - every finding, the record's and any other layer's on the entry, is an
///    `unknown` the capture attributes to the declaration (`declared`), at a
///    position under a named member. `unknown[]` or `Record<string, unknown>`
///    at the root has no named member, and says nothing about the payload.
///
/// This decides the entry's state only. What the compatibility check makes of
/// an open member is the check phase's own walk, which reads the same record
/// and is unchanged.
pub(crate) fn top_types_are_declared_open_members(
    expanded: &str,
    record: &CaptureAliasRecord,
    published: &[crate::services::type_sidecar::TypeProvenance],
) -> bool {
    !scrubbed_text_uses_keyword_as_type(&strip_string_literal_contents(expanded), "any")
        && !record.any_provenance.is_empty()
        && record.any_provenance.len() < CAPTURE_FINDINGS_CAP
        && record
            .any_provenance
            .iter()
            .chain(published)
            .all(is_declared_open_member)
}

/// An `unknown` the declaration states, under a named member.
fn is_declared_open_member(finding: &crate::services::type_sidecar::TypeProvenance) -> bool {
    finding.kind == "unknown" && finding.reason == "declared" && path_names_a_member(&finding.path)
}

/// True when a deep-walk path passes through a named member. The walk writes
/// a member as its name (`a`, `a.b`), an element or type argument as `<n>`, an
/// index signature as `[index]` and a callable return as `()`; a path made of
/// the last three alone sits at the root of the type or inside a container,
/// never in a field.
fn path_names_a_member(path: &str) -> bool {
    let rest = path.replace("[index]", "").replace("()", "");
    let mut in_argument = false;
    rest.chars().any(|c| match c {
        '<' => {
            in_argument = true;
            false
        }
        '>' => {
            in_argument = false;
            false
        }
        '.' => false,
        _ => !in_argument,
    })
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
///
/// A call result's root `machinery_envelope` is a decision too: what the
/// call's result carrier holds is transport the service's wrapper rules verify
/// and read no payload out of, such as a request library's own response object
/// (carrick#1841). The capture's re-run would publish the carrier. A handler
/// return with that reason states that some of its union branches were unread
/// (carrick#166), and keeps the capture's own read as its recovery.
fn inference_decided_no_contract(inf: &crate::services::type_sidecar::InferredType) -> bool {
    text_is_bare_top_type(&inf.type_string)
        && inf.any_provenance.iter().any(|p| {
            p.path.is_empty()
                && (DECIDED_ABSTAIN_REASONS.contains(&p.reason.as_str())
                    || (inf.infer_kind == InferKind::CallResult
                        && p.reason == "machinery_envelope"))
        })
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
    // first-wins `or_insert`), and the unwidened reading of that same
    // inference when it has one (carrick#1516).
    let mut inferred_text: HashMap<&str, &str> = HashMap::new();
    let mut unwidened_text: HashMap<&str, &str> = HashMap::new();
    // carrick#1836: what that inference's bare names meant, for both texts.
    let mut printed_names: HashMap<&str, &[crate::services::type_sidecar::PrintedName]> =
        HashMap::new();
    // carrick#1842: the aliases whose published text is a body read as raw
    // text, marked by the same inference that supplied the text.
    let mut raw_text: HashSet<&str> = HashSet::new();
    for inf in inferred {
        if let Some(text) = usable_inferred_text(&inf.type_string) {
            if inferred_text.contains_key(inf.alias.as_str()) {
                continue;
            }
            inferred_text.insert(inf.alias.as_str(), text);
            printed_names.insert(inf.alias.as_str(), &inf.printed_names);
            if inf.raw_text_read {
                raw_text.insert(inf.alias.as_str());
            }
            if let Some(unwidened) = inf
                .unwidened_type_string
                .as_deref()
                .and_then(usable_inferred_text)
                .filter(|unwidened| *unwidened != text)
            {
                unwidened_text.insert(inf.alias.as_str(), unwidened);
            }
        }
    }
    let modes = inferred_response_modes(inferred);
    // carrick#2054: each case of an alias's modes, captured once, beside
    // whichever anchor publishes the alias.
    let mut moded: HashSet<&str> = HashSet::new();
    let mut mode_anchors = |alias: &str, request: &InferRequestItem| -> Vec<CaptureAnchor> {
        let Some((alias, (found, texts))) = modes.get_key_value(alias) else {
            return Vec::new();
        };
        if !moded.insert(*alias) {
            return Vec::new();
        }
        let names = printed_names
            .get(alias)
            .map_or_else(Vec::new, |names| names.to_vec());
        let mut anchors: Vec<CaptureAnchor> = found
            .cases
            .iter()
            .zip(texts)
            .map(|(case, text)| CaptureAnchor::Literal {
                alias: case.alias.clone(),
                type_text: (*text).to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: Some(repo_relative(&request.file_path, repo_root)),
                printed_names: names.clone(),
                raw_text_read: false,
            })
            .collect();
        anchors.sort_by(|a, b| a.alias().cmp(b.alias()));
        anchors
    };
    let sightings = Sightings::of(inferred);
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
        // back with no shape and no sight of this symbol there is no depth to
        // copy — and no way to tell "the type is scalar" from "nothing was
        // seen" (carrick#1967 for the answer that names the transport's
        // response object beside a bare top type). Capturing the bare symbol anyway
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
        if request.array_depth.is_none() && sightings.none_of(alias, &request.symbol_name) {
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
            consumer_response: request.consumer_response,
        });
    }

    for request in infer {
        let Some(alias) = request.alias.as_deref() else {
            continue;
        };
        if !seen.insert(alias.to_string()) {
            // A symbol anchor publishes the alias; its modes are still what
            // the handler sends for each value.
            anchors.extend(mode_anchors(alias, request));
            continue;
        }
        // Kind-aware v1 inference result wins over a raw locator re-run.
        if let Some(text) = inferred_text.get(alias) {
            let names = printed_names
                .get(alias)
                .map_or_else(Vec::new, |names| names.to_vec());
            let unwidened = unwidened_text
                .get(alias)
                .map(|unwidened| CaptureAnchor::Literal {
                    alias: unwidened_alias(alias),
                    type_text: (*unwidened).to_string(),
                    anchor_origin: AnchorOrigin::DeterministicInfer,
                    source_file: Some(repo_relative(&request.file_path, repo_root)),
                    printed_names: names.clone(),
                    raw_text_read: false,
                });
            anchors.push(CaptureAnchor::Literal {
                alias: alias.to_string(),
                type_text: (*text).to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: Some(repo_relative(&request.file_path, repo_root)),
                printed_names: names,
                raw_text_read: raw_text.contains(alias),
            });
            anchors.extend(unwidened);
            anchors.extend(mode_anchors(alias, request));
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
                printed_names: Vec::new(),
                raw_text_read: false,
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
            printed_names: Vec::new(),
            raw_text_read: false,
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
            printed_names: Vec::new(),
            raw_text_read: false,
        });
    }

    anchors
}

/// Why a service's capture produced no stub (carrick#1921). The caller writes
/// it on the service, so the index says what happened without anyone probing
/// the sidecar afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    /// The sidecar gave no answer and reported no progress inside the
    /// operation deadline. It was killed and replaced (carrick#1914). Not
    /// retried: the same request would wait the same deadline out.
    TimedOut,
    /// The sidecar process ended before it answered, in the process the
    /// capture was first asked of and again in the fresh one it was retried
    /// in; or no process could be started to ask. The text says which, and
    /// for two deaths how each process ended and at which stage of the
    /// capture (`two_deaths`).
    SidecarDied(String),
    /// The sidecar answered, and the answer was not a stub: the capture's own
    /// error, or a stub package this side could not read.
    Failed(String),
}

impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureFailure::TimedOut => write!(
                f,
                "the type sidecar gave no answer and reported no progress inside the \
                 operation deadline"
            ),
            CaptureFailure::SidecarDied(detail) | CaptureFailure::Failed(detail) => {
                write!(f, "{detail}")
            }
        }
    }
}

/// Run `capture_v2` for one service and read the stub package into the wire
/// artifact. Returns the on-disk stub dir (for the definitions re-point;
/// caller owns cleanup) alongside the artifact. `Err` = capture degraded, and
/// why; the service ships without a surface and its pairs verdict
/// unverifiable.
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
) -> Result<(PathBuf, CaptureStubArtifact), CaptureFailure> {
    if anchors.is_empty() {
        return Err(CaptureFailure::Failed("no anchors to capture".to_string()));
    }
    let repo_root = std::path::Path::new(repo_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(repo_path));

    let (stub_dir, artifact, records) =
        capture_once(sidecar, &repo_root, service_id, anchors, tsconfig_path)?;

    let Some((rerun_anchors, backfilled)) = backfill_anchors(anchors, &records, backfill_texts)
    else {
        return Ok((stub_dir, artifact));
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
        Ok((rerun_dir, rerun_artifact, rerun_records))
            if backfill_accepted(&backfilled, &records, &rerun_records) =>
        {
            debug!(
                "v2 capture for {}: backfill adopted ({} alias(es) re-anchored)",
                service_id,
                backfilled.len()
            );
            let _ = std::fs::remove_dir_all(&stub_dir);
            Ok((rerun_dir, rerun_artifact))
        }
        Ok((rerun_dir, _, _)) => {
            // Fail-closed: the backfill result failed its self-check or
            // degraded a sibling alias — keep the demoted/unknown surface
            // (honest unverifiable), never a backfill that didn't verify.
            debug!(
                "v2 capture for {}: backfill rejected by self-check; keeping demoted surface",
                service_id
            );
            let _ = std::fs::remove_dir_all(&rerun_dir);
            Ok((stub_dir, artifact))
        }
        // The first capture stands: the backfill is an extra, and its own
        // failure is in the log.
        Err(_) => Ok((stub_dir, artifact)),
    }
}

/// One capture: ask it of a fresh sidecar process, and read the stub package
/// it wrote into the wire artifact, keeping the per-alias records (the
/// demotion signal the backfill decision needs).
fn capture_once(
    sidecar: &TypeSidecar,
    repo_root: &Path,
    service_id: &str,
    anchors: &[CaptureAnchor],
    tsconfig_path: Option<&str>,
) -> Result<(PathBuf, CaptureStubArtifact, Vec<CaptureAliasRecord>), CaptureFailure> {
    let result = capture_in_a_fresh_process(sidecar, repo_root, service_id, anchors, tsconfig_path)
        .inspect_err(|failure| warn!("v2 capture failed for {}: {}", service_id, failure))?;

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
        Ok(artifact) => Ok((stub_dir, artifact, result.aliases)),
        Err(e) => {
            warn!("failed to read capture stub for {}: {}", service_id, e);
            let _ = std::fs::remove_dir_all(&stub_dir);
            Err(CaptureFailure::Failed(format!(
                "the stub package the capture wrote could not be read: {e}"
            )))
        }
    }
}

/// Ask one `capture_v2` of a sidecar process started for it, and once more of
/// another if that process dies (carrick#1916).
///
/// **A process of its own.** By the time a service is captured its sidecar has
/// built the service's program and served every inference over it, and holds
/// all of that. The capture reads none of it: it builds its own program from
/// the service's tsconfig. On a 2,345-file project that was 3.1 GB of live
/// heap under a capture that added 3.4 GB, against an 8 GB cap; the same
/// request alone in a fresh process answered at 2.8 GB. So the sidecar is
/// replaced first, by the restart a timed-out operation already uses, scoped
/// as it was. Every capture gets one, the backfill re-capture included: a
/// process that has just captured still holds part of that capture.
///
/// **One retry.** A process that dies with the capture in hand (out of heap,
/// killed by the OS) takes nothing of the service with it, so the capture is
/// asked once more of another fresh process before the service ships with no
/// surface. A timeout is not retried, and neither is an answer that says the
/// capture failed: both would come back the same.
///
/// **A live sidecar afterwards, whatever happened.** After a second death one
/// more process is started and left in place, because the sidecar serves the
/// rest of the scan: the next service's init and the check phase would
/// otherwise write to a closed pipe, and one service's capture would cost
/// every later service its types.
///
/// **What the two deaths were.** A capture reports each stage as it reaches
/// it, so a process that dies leaves the stage it had reached, and the OS
/// says how it ended. Both go into the failure (`two_deaths`): the index then
/// says whether the heap ran out and on what, where it used to say only that
/// the process was gone.
fn capture_in_a_fresh_process(
    sidecar: &TypeSidecar,
    repo_root: &Path,
    service_id: &str,
    anchors: &[CaptureAnchor],
    tsconfig_path: Option<&str>,
) -> Result<CaptureV2Result, CaptureFailure> {
    let not_started = |e: SidecarError| CaptureFailure::SidecarDied(e.to_string());
    let mut died_before: Option<CaptureDeath> = None;
    loop {
        sidecar
            .restart(match died_before {
                None => "a capture runs in a process of its own",
                Some(_) => "the sidecar died during a capture, which is retried once",
            })
            .map_err(not_started)?;

        let out_dir = std::env::temp_dir().join(format!(
            "carrick-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let error = match sidecar.capture_v2(
            &repo_root.to_string_lossy(),
            service_id,
            anchors,
            &out_dir.to_string_lossy(),
            tsconfig_path,
        ) {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let _ = std::fs::remove_dir_all(&out_dir);

        if !error.is_process_death() {
            return Err(match error {
                SidecarError::Timeout => CaptureFailure::TimedOut,
                SidecarError::CaptureFailed(detail) => CaptureFailure::Failed(detail),
                other => CaptureFailure::Failed(other.to_string()),
            });
        }
        // Read before anything else is asked of the sidecar: the next wait
        // starts the progress empty, and the restart replaces the process.
        let death = CaptureDeath {
            error: error.to_string(),
            progress: sidecar.last_progress(),
            end: sidecar.how_the_process_ended(),
        };
        let Some(first) = died_before.replace(death.clone()) else {
            warn!(
                "The type sidecar died during the capture for {} ({}): it {} {}; retrying \
                 once in a fresh process",
                service_id,
                error,
                death.ended(false),
                death.stage()
            );
            continue;
        };
        // Whether or not this one starts, the capture's failure is the two
        // deaths; a failure to start is left in the sidecar's state for
        // whoever asks next.
        let _ = sidecar.restart("the sidecar died during a capture and during its retry");
        return Err(CaptureFailure::SidecarDied(two_deaths(
            &first,
            &death,
            crate::services::type_sidecar::heap_cap_mb(),
        )));
    }
}

/// What is known about a capture process that ended before it answered.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CaptureDeath {
    /// The client's own error for it.
    error: String,
    /// The last stage the capture reported, when it reported any.
    progress: Option<OperationProgress>,
    /// How the process ended, when the OS said.
    end: Option<ProcessEnd>,
}

impl CaptureDeath {
    /// How the process ended, as the verb of a sentence about one process or
    /// about `both`; plain "ended" when the OS did not say.
    fn ended(&self, both: bool) -> String {
        let was = if both { "were" } else { "was" };
        match self.end {
            Some(end) if end.is_abort() => {
                format!("{was} aborted (SIGABRT), as Node aborts when its heap is full,")
            }
            Some(ProcessEnd::Signal(9)) => format!("{was} killed (SIGKILL)"),
            Some(ProcessEnd::Signal(signal)) => format!("{was} ended by signal {signal}"),
            Some(ProcessEnd::Code(code)) => format!("ended with exit code {code}"),
            None => "ended".to_string(),
        }
    }

    /// The stage of the capture the process had reached.
    fn stage(&self) -> String {
        let Some(progress) = &self.progress else {
            return "before reporting any progress".to_string();
        };
        match progress.phase.as_str() {
            "program" => {
                "while building the service's program, before any anchor was resolved".to_string()
            }
            "anchors" => format!(
                "while resolving anchors (last report: {})",
                progress.message
            ),
            "emit" => "while emitting declarations, after every anchor was resolved".to_string(),
            "self-check" => "while type-checking the emitted declarations, after every anchor \
                             was resolved"
                .to_string(),
            other => format!("at stage '{other}' ({})", progress.message),
        }
    }

    /// What the stage held in the heap, for the stages that say: the thing
    /// that did not fit when the process aborted there.
    fn held(&self) -> Option<&'static str> {
        match self.progress.as_ref()?.phase.as_str() {
            "program" => Some("the service's program"),
            "anchors" => Some("the service's program and the types of its anchors"),
            "emit" => Some("the declarations being emitted"),
            "self-check" => Some("the emitted declarations and the packages they import"),
            _ => None,
        }
    }
}

/// The failure of a capture whose process ended twice: the two deaths as the
/// client saw them, then how each process ended and at which stage, then,
/// when both aborted at one stage, what did not fit the heap they were given.
///
/// `heap_cap_mb` is the cap every sidecar of this run is started with, or
/// `None` when it runs under V8's own default.
fn two_deaths(first: &CaptureDeath, retry: &CaptureDeath, heap_cap_mb: Option<u64>) -> String {
    let lead = format!(
        "the type sidecar process ended during the capture ({}) and again when the capture \
         was retried in a fresh process ({})",
        first.error, retry.error
    );
    let (heap, under) = match heap_cap_mb {
        Some(mb) => (
            format!("the {mb} MB heap it was given"),
            format!("under a heap cap of {mb} MB"),
        ),
        None => (
            "V8's default heap".to_string(),
            "under V8's default heap cap".to_string(),
        ),
    };
    let same = first.end == retry.end && first.stage() == retry.stage();
    if !same {
        return format!(
            "{lead}; the first {} {}, the retry {} {}, {under}",
            first.ended(false),
            first.stage(),
            retry.ended(false),
            retry.stage()
        );
    }
    let both = format!("{lead}; both {} {}", first.ended(true), first.stage());
    match first
        .held()
        .filter(|_| first.end.is_some_and(|end| end.is_abort()))
    {
        Some(held) => format!("{both}: {held} did not fit {heap}"),
        None => format!("{both}, {under}"),
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
///
/// The explicit bundle's text is a model symbol's own expansion, so it is
/// offered only where the symbol anchor is (carrick#1967): an alias at which
/// nothing witnessed the symbol, and for which the caller knows no depth, gets
/// no text from it. Its symbol anchor was withheld because its array-ness
/// would be a guess, and re-anchoring the alias with the same symbol's text
/// publishes the same guess one step later.
pub(crate) fn derive_backfill_texts(
    explicit_manifest: &[ManifestEntry],
    inferred: &[crate::services::type_sidecar::InferredType],
    explicit: &[SymbolRequest],
) -> HashMap<String, String> {
    let sightings = Sightings::of(inferred);
    let request_of: HashMap<&str, &SymbolRequest> = explicit
        .iter()
        .filter_map(|request| Some((request.alias.as_deref()?, request)))
        .collect();
    let mut texts: HashMap<String, String> = HashMap::new();
    for entry in explicit_manifest {
        let request = request_of.get(entry.alias.as_str());
        let symbol = request.map_or(entry.original_name.as_str(), |r| r.symbol_name.as_str());
        let depth_known = request.is_some_and(|r| r.array_depth.is_some());
        if !depth_known && sightings.none_of(&entry.alias, symbol) {
            continue;
        }
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
                    printed_names: Vec::new(),
                    raw_text_read: false,
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

/// Whether the call has a placeholder at a position where the route states a
/// literal segment (carrick#2004): `PATCH /things/${id}` against
/// `PATCH /things/move`, or `POST /orgs/${id}/${target}` against every
/// `POST /orgs/:orgId/<literal>`. The call's value there is unknown, so the
/// pair is a guess that the call reaches this route and not another, and it
/// is not compared. A route placeholder over a call literal is the ordinary
/// case (`/users/:id` serves `/users/me`) and is not a guess.
fn call_placeholder_over_route_literal(producer_path: &str, consumer_path: &str) -> bool {
    let route = normalize_match_path(producer_path);
    let call = normalize_match_path(consumer_path);
    route
        .split('/')
        .zip(call.split('/'))
        .any(|(route_seg, call_seg)| call_seg.starts_with(':') && !route_seg.starts_with(':'))
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
    /// The producer's response as its handler returns it, no literal widened
    /// (`unwidened_definition`, carrick#1516). The retype states it when the
    /// published type raised diagnostics, to tell a producer type wider than
    /// what it sends from a break.
    pub producer_unwidened: Option<String>,
    /// The consumer's published type for the same kind. With the producer's,
    /// it says whether either side states a request body at all.
    pub consumer_expanded: Option<String>,
    /// Set when the producer's response depends on a field of the incoming
    /// message and the call states no value that picks one case
    /// (carrick#2054): why a mismatch on this pair is not reported. The pair
    /// is still judged against the whole union, and only an incompatible
    /// verdict is published unverifiable ([`abstain_on_unstated_modes`]).
    pub unstated_mode: Option<String>,
}

/// What a call's stated message picks from a producer response's modes
/// (carrick#2054).
#[derive(Debug, PartialEq, Eq)]
enum ModeSelection<'a> {
    /// The response does not depend on the message: judged as published.
    NoModes,
    /// The call states the value of the one field the handler reads, and the
    /// handler has a body for it: judged against that case alone.
    Narrowed(&'a crate::cloud_storage::ResponseModeCase, &'a str),
    /// The response depends on the message and nothing the call states
    /// picks one case. The reason says why.
    Unstated(String),
}

/// The sentence a call reads when the route's response depends on the value
/// of one field of the request and the call states none of the values the
/// handler distinguishes (carrick#2054).
fn mode_unstated_reason(field: &str, cases: &[crate::cloud_storage::ResponseModeCase]) -> String {
    let values = cases
        .iter()
        .filter_map(|case| case.value.as_deref())
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Not compared: this route returns a different response for each value of `{field}` in the request ({values}), and this call does not set `{field}` to one of them."
    )
}

/// Pick the case of `modes` a call that states `stated` receives
/// (carrick#2054).
///
/// - No modes, or modes that read no field: [`ModeSelection::NoModes`].
/// - One placed field with cases: the case the stated value names, else the
///   case for any other value. A value with no case, or no value stated in
///   the field's own location, is unstated.
/// - Anything else (an unplaced read, two fields, a field whose values the
///   handler tests in a way not read), or a case the capture could not
///   publish: unstated, since no case can be told apart from the others.
fn select_mode<'a>(
    modes: Option<&'a crate::cloud_storage::ResponseModes>,
    stated: &crate::cloud_storage::StatedValues,
) -> ModeSelection<'a> {
    let Some(modes) = modes.filter(|modes| !modes.reads.is_empty()) else {
        return ModeSelection::NoModes;
    };
    let fields = || {
        let mut fields: Vec<&str> = modes.reads.iter().map(|read| read.field.as_str()).collect();
        fields.sort_unstable();
        fields.dedup();
        fields
    };
    let read = match modes.reads.as_slice() {
        [read]
            if read.location != crate::services::type_sidecar::MessageSource::Unplaced
                && !modes.cases.is_empty() =>
        {
            read
        }
        _ => return ModeSelection::Unstated(case_unknown_reason(&fields())),
    };
    let value = stated
        .get(&read.location)
        .and_then(|values| values.get(&read.field));
    let case = value.and_then(|value| {
        modes
            .cases
            .iter()
            .find(|case| case.value.as_deref() == Some(value.as_str()))
            .or_else(|| modes.cases.iter().find(|case| case.value.is_none()))
    });
    match case {
        Some(case) => match case.expanded.as_deref() {
            Some(expanded) => ModeSelection::Narrowed(case, expanded),
            None => ModeSelection::Unstated(case_unknown_reason(&fields())),
        },
        None => ModeSelection::Unstated(mode_unstated_reason(&read.field, &modes.cases)),
    }
}

struct ServiceEntry<'a> {
    service_id: &'a str,
    has_surface: bool,
    entry: &'a TypeManifestEntry,
    /// The dispatch case of every row this entry stands for (carrick#831):
    /// on a producer, each case the route answers at this site (several cases
    /// with no site of their own share the route's, and so its alias); on a
    /// consumer, the case each call at this site sends. `[None]` is a plain
    /// route, or a call that states no case, which is nearly every entry.
    cases: Vec<Option<&'a crate::dispatch::Dispatch>>,
}

/// The sentence a call to a route that dispatches on a request field reads
/// when the call states no case (carrick#2059). The analyzer keeps its edge
/// to the route with the case unknown (`carrick_match::dispatch_outcome`), so
/// the half is stored, and nothing was compared.
fn case_unknown_reason(fields: &[&str]) -> String {
    let fields = fields
        .iter()
        .map(|field| format!("`{field}`"))
        .collect::<Vec<_>>()
        .join(" and ");
    format!(
        "Not compared: this route returns a different response depending on {fields} in the request, and Carrick cannot tell which one this call receives."
    )
}

/// Each HTTP row's dispatch case, by the site and operation a manifest entry
/// names. Joined on `(file, line, method, path)`, the identity
/// [`build_type_manifest_entries`](crate::engine) builds the entry from.
type CasesBySite<'a> =
    HashMap<(String, u32, String, String), Vec<Option<&'a crate::dispatch::Dispatch>>>;

fn cases_by_site(rows: &[crate::analyzer::ApiEndpointDetails]) -> CasesBySite<'_> {
    let mut by_site: CasesBySite = HashMap::new();
    for row in rows {
        let Some((method, path)) = row.key.as_http() else {
            continue;
        };
        let (file, line) =
            crate::type_manifest::parse_file_location(&row.file_path.to_string_lossy());
        by_site
            .entry((
                file,
                line,
                crate::type_manifest::normalize_manifest_method(method),
                path.to_string(),
            ))
            .or_default()
            .push(row.dispatch.as_ref());
    }
    by_site
}

/// What one candidate producer entry answers a consumer entry about the
/// dispatch case, folded over the cases on each side
/// (`carrick_match::dispatch_verdict`): any pair of cases that matches, or a
/// producer that does not dispatch, is a match; else any case the call leaves
/// unknown is unknown; else the call states a value no case here answers.
fn entry_dispatch_verdict(
    producer: &ServiceEntry,
    consumer: &ServiceEntry,
) -> carrick_match::DispatchVerdict {
    use carrick_match::DispatchVerdict;
    let key = |case: &Option<&crate::dispatch::Dispatch>| crate::dispatch::Dispatch::key_of(*case);
    let mut verdict = DispatchVerdict::ValueMismatch;
    for producer_case in &producer.cases {
        for consumer_case in &consumer.cases {
            match carrick_match::dispatch_verdict(key(producer_case), key(consumer_case)) {
                DispatchVerdict::Matched | DispatchVerdict::NotDispatching => {
                    return DispatchVerdict::Matched;
                }
                DispatchVerdict::ValueUnknown => verdict = DispatchVerdict::ValueUnknown,
                DispatchVerdict::ValueMismatch => {}
            }
        }
    }
    verdict
}

/// Build check pairs from every participating repo's manifest.
///
/// Port of the ts_check manifest-matcher pairing semantics:
/// - HTTP: method + route-aware path match + type_kind, keeping only the
///   most specific producer(s) per consumer. A route that states a literal
///   where the call has a placeholder is never a candidate: which route the
///   call reaches is then unknown, and the call is not compared
///   (carrick#2004). The consumer's own service is a
///   candidate like any other (carrick#1945): a call to a route of its own
///   service is a consumer of that route (carrick#1926), and the HTTP matcher
///   keeps that edge (carrick#1944). Its routes are ranked with every
///   sibling's by the one specificity score, so a sibling's literal route
///   wins over the caller's own parameterized one, and the other way round.
///   Among those, a route that dispatches on a request field is decided case
///   by case as the analyzer's matcher decides it
///   (`carrick_match::dispatch_outcome`, carrick#2059): a call is paired with
///   the case it sends, a call that sends a value no case answers is not
///   paired, and a call that states no case gets one half per kind, stored
///   unverifiable without a probe.
/// - socket/graphql/pubsub: exact operation-key match + type_kind, between
///   two services only. The exact-key matcher drops a same-service edge for
///   these protocols (#397/#410), so a pair here would be judged and stored
///   against no edge.
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
        let endpoint_cases = cases_by_site(&repo.endpoints);
        let call_cases = cases_by_site(&repo.calls);
        for entry in entries {
            let (target, rows) = match entry.role {
                ManifestRole::Producer => (&mut producers, &endpoint_cases),
                ManifestRole::Consumer => (&mut consumers, &call_cases),
            };
            let cases = entry
                .key
                .as_http()
                .and_then(|(method, path)| {
                    rows.get(&(
                        entry.file_path.clone(),
                        entry.line_number,
                        method.to_string(),
                        path.to_string(),
                    ))
                })
                .cloned()
                .unwrap_or_else(|| vec![None]);
            target.push(ServiceEntry {
                service_id,
                has_surface,
                entry,
                cases,
            });
        }
    }

    let mut pairs: Vec<BuiltPair> = Vec::new();
    for consumer in &consumers {
        // Candidate producers, protocol-dispatched.
        let mut candidates: Vec<(&ServiceEntry, u8)> = Vec::new();
        // Routes the call could only be paired with by guessing its
        // placeholder's value (carrick#2004).
        let mut guessed: Vec<&str> = Vec::new();
        for producer in &producers {
            // One service on both ends is a pair for HTTP only (see above).
            if producer.service_id == consumer.service_id && consumer.entry.key.as_http().is_none()
            {
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
                    if call_placeholder_over_route_literal(pp, cp) {
                        guessed.push(pp.as_str());
                        continue;
                    }
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
            if !guessed.is_empty() {
                guessed.sort_unstable();
                guessed.dedup();
                debug!(
                    "v2 check: {} {} at {}:{} is not compared: its placeholder stands where {} route(s) state a literal segment ({})",
                    consumer
                        .entry
                        .key
                        .as_http()
                        .map_or("", |(method, _)| method),
                    consumer.entry.key.as_http().map_or("", |(_, path)| path),
                    consumer.entry.file_path,
                    consumer.entry.line_number,
                    guessed.len(),
                    guessed.join(", ")
                );
            }
            continue;
        }
        // HTTP specificity: keep only the best-scoring producer(s), mirroring
        // routing semantics (a literal route wins over :param).
        let best = candidates.iter().map(|(_, s)| *s).max().unwrap_or(0);
        // Then the dispatch case, as the analyzer's matcher decides it after
        // the same specificity filter (carrick#2059): the check pairs a call
        // with exactly the producers its edge reaches, so a half it judges is
        // a half the index stores.
        let mut matched: Vec<&ServiceEntry> = Vec::new();
        let mut case_unknown: Vec<&ServiceEntry> = Vec::new();
        let mut case_mismatched = 0u32;
        for (producer, score) in candidates {
            if score != best {
                continue;
            }
            match entry_dispatch_verdict(producer, consumer) {
                carrick_match::DispatchVerdict::ValueUnknown => case_unknown.push(producer),
                carrick_match::DispatchVerdict::ValueMismatch => case_mismatched += 1,
                _ => matched.push(producer),
            }
        }
        match carrick_match::dispatch_outcome(
            matched.len() as u32,
            case_unknown.len() as u32,
            case_mismatched,
        ) {
            carrick_match::DispatchOutcome::Matched => {
                pairs.extend(
                    matched
                        .into_iter()
                        .filter_map(|producer| build_pair(producer, consumer)),
                );
            }
            // The call keeps one edge to the route with its case unknown, so
            // it gets one half per kind, stored unverifiable without a probe:
            // which case's type to compare against is the thing not known.
            carrick_match::DispatchOutcome::RouteCaseUnknown => {
                case_unknown.sort_by(|a, b| {
                    (a.service_id, &a.entry.type_alias).cmp(&(b.service_id, &b.entry.type_alias))
                });
                let mut fields: Vec<&str> = case_unknown
                    .iter()
                    .flat_map(|producer| producer.cases.iter().flatten())
                    .map(|case| case.field.as_str())
                    .collect();
                fields.sort_unstable();
                fields.dedup();
                if let Some(mut pair) = build_pair(case_unknown[0], consumer) {
                    pair.pre_verdict =
                        Some((VerdictBucket::Unverifiable, case_unknown_reason(&fields)));
                    pair.pre_verdict_side = None;
                    pairs.push(pair);
                }
            }
            // A value no case answers, as the matcher: no edge, so no pair.
            carrick_match::DispatchOutcome::NoCaseAnswers
            | carrick_match::DispatchOutcome::NotMatched => {}
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

    // carrick#2054: a response the handler chooses by a field of the
    // incoming message is judged against the case the call states. The pair
    // keeps the published alias's key, so its verdict lands where every other
    // verdict for this call does.
    let mut probed_alias = producer.entry.type_alias.clone();
    let mut producer_expanded = producer.entry.expanded_definition.clone();
    let mut producer_unwidened = producer.entry.unwidened_definition.clone();
    let mut unstated_mode = None;
    if producer.entry.type_kind == ManifestTypeKind::Response {
        match select_mode(
            producer.entry.response_modes.as_ref(),
            &consumer.entry.stated_values,
        ) {
            ModeSelection::NoModes => {}
            ModeSelection::Narrowed(case, expanded) => {
                probed_alias = case.alias.clone();
                producer_expanded = Some(expanded.to_string());
                producer_unwidened = None;
            }
            ModeSelection::Unstated(reason) => unstated_mode = Some(reason),
        }
    }

    Some(BuiltPair {
        spec: CheckPairSpec {
            pair_key,
            protocol,
            type_kind,
            producer: CheckPairEndpoint {
                service_name: producer.service_id.to_string(),
                alias: probed_alias,
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
        producer_expanded,
        producer_unwidened,
        consumer_expanded: consumer.entry.expanded_definition.clone(),
        unstated_mode,
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
/// never reach the sidecar. A check that failed part way (an install, an
/// abnormal tsc) still returns each pair's verdict: the pairs it stopped are
/// unverifiable and say why, and the pairs the capture had already decided
/// keep that verdict. Only when the sidecar returns no verdicts at all does
/// every probing pair degrade to unverifiable with the failure as the reason.
/// Never fatal to the scan, and never read as compatible.
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
                if !result.success {
                    warn!(
                        "v2 check failed; each pair keeps the verdict the sidecar returned: {}",
                        result.errors.join("; ")
                    );
                }
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
                    "v2 check returned no verdicts; all probing pairs degrade to unverifiable: {}",
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
    abstain_on_unstated_modes(&pairs, &mut outcomes);
    hold_back_same_service_mismatches(&mut outcomes);

    // Deterministic order for every downstream consumer.
    outcomes.sort_by(|a, b| a.pair_key.cmp(&b.pair_key));
    log_unresolved_pairs(&outcomes, &pairs, local_consumers);
    outcomes
}

/// The gate a mismatch against a response whose case the call does not
/// state is stamped with (carrick#2054).
const UNSTATED_MODE_GATE: &str = "producer:mode";

/// Publish every pair [`BuiltPair::unstated_mode`] marks that the check found
/// incompatible as unverifiable, with the pair's reason (carrick#2054).
///
/// The producer's handler sends a different body for each value of a field
/// of the incoming message, and the call states no value that picks one, so
/// the pair was judged against the union of them all. A read that fails
/// against that union may be a read of the case this call receives: not a
/// break anyone can act on. A compatible pair holds for every case and is
/// kept. Like [`hold_back_same_service_mismatches`], what the check found
/// stays as the outcome's `diagnostic` for the run log only.
fn abstain_on_unstated_modes(pairs: &[BuiltPair], outcomes: &mut [PairCheckOutcome]) {
    let unstated: HashMap<&str, &str> = pairs
        .iter()
        .filter_map(|pair| Some((pair.spec.pair_key.as_str(), pair.unstated_mode.as_deref()?)))
        .collect();
    for outcome in outcomes.iter_mut() {
        if outcome.bucket != VerdictBucket::Incompatible {
            continue;
        }
        let Some(reason) = unstated.get(outcome.pair_key.as_str()) else {
            continue;
        };
        outcome.bucket = VerdictBucket::Unverifiable;
        outcome.gate = Some(UNSTATED_MODE_GATE.to_string());
        outcome.diagnostic = outcome.diagnostic.take().filter(|found| !found.is_empty());
        outcome.resolved = false;
        outcome.unresolved_reason = Some((*reason).to_string());
        outcome.notes.clear();
        outcome.consumer_reads.clear();
    }
}

/// Why a same-service half the check found incompatible is published as
/// unverifiable (carrick#1945). The cloud's readers quote it, so it is one
/// sentence, stated once.
pub(crate) const SAME_SERVICE_MISMATCH_NOT_REPORTED: &str =
    "a mismatch between a call and a route of its own service, which Carrick does not report yet";

/// The gate a held-back same-service mismatch is stamped with.
const SAME_SERVICE_GATE: &str = "same_service";

/// Publish every same-service pair the check found incompatible as
/// unverifiable, saying why ([`SAME_SERVICE_MISMATCH_NOT_REPORTED`]).
///
/// Ruled for the first release that pairs a service with itself
/// (carrick#1945): hand-checked on a one-service app, most of those
/// mismatches came from the way a route's response is read (error sends
/// joined to the success body, one handler's methods joined), not from a
/// call that breaks. Until those causes are fixed a same-service mismatch is
/// not a fact a pull request should fail on. A same-service `compatible`, an
/// unverifiable half and every pair of two services are unchanged.
///
/// What the check found stays as the outcome's `diagnostic` (carrick#2053; a
/// retype mismatch's text already names the consumer's lines), and the run
/// log prints it after the sentence. Nothing a reader of the index sees
/// changes: the stored half is unverifiable with the sentence as its reason,
/// no `reason` and no notes, because a note reaches agents and a held-back
/// mismatch is not for them. The consumer's lines are cleared as such, since
/// a finding projects them as the places of a mismatch.
///
/// Done here, on the outcomes, because every reader starts from them: the
/// stored verdict rows, the edges' `type_compatible`, and the findings a pull
/// request's output is written from.
fn hold_back_same_service_mismatches(outcomes: &mut [PairCheckOutcome]) {
    for outcome in outcomes.iter_mut() {
        if outcome.producer_service != outcome.consumer_service
            || outcome.bucket != VerdictBucket::Incompatible
        {
            continue;
        }
        outcome.bucket = VerdictBucket::Unverifiable;
        outcome.gate = Some(SAME_SERVICE_GATE.to_string());
        outcome.diagnostic = outcome.diagnostic.take().filter(|found| !found.is_empty());
        outcome.resolved = false;
        outcome.unresolved_reason = Some(SAME_SERVICE_MISMATCH_NOT_REPORTED.to_string());
        outcome.notes.clear();
        outcome.consumer_reads.clear();
    }
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
/// all (it reads no body, sends a form body, reads or sends bytes,
/// carrick#1793, or reads the body as raw text, carrick#1842) has nothing to
/// compare.
///
/// A proven mismatch is never a candidate: the retype may only lift a verdict
/// that compared nothing, never downgrade one that found a break.
fn consumer_to_blame(verdict: &crate::services::type_sidecar::CheckVerdict) -> bool {
    verdict.bucket != VerdictBucket::Incompatible
        && verdict.unresolved_side == Some(VerdictSide::Consumer)
        && !verdict.gate.as_deref().is_some_and(|gate| {
            gate.ends_with(":void")
                || gate.ends_with(":form")
                || gate.ends_with(":bytes")
                || gate.ends_with(":text")
        })
}

/// Said on every pair the retype decided.
const RETYPE_NOTE: &str = "judged by retyping the consumer's call with the producer's response \
     type and type-checking the consumer's own code";

/// Said on a pair whose producer type is wider than what its handler returns
/// (carrick#1516), before the places the published type fails.
const PRODUCER_WIDER_NOTE: &str = "the producer's declared response is wider than what its \
     handler returns: TypeScript widened a literal the handler returns, and the consumer \
     accepts every value the handler sends";

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
                producer_unwidened_type: pair
                    .producer_unwidened
                    .as_deref()
                    .filter(|text| {
                        !contains_disqualifying_top_type(text) && !text.trim().is_empty()
                    })
                    .map(str::to_string),
                wire: true,
            });
    }

    by_service
}

/// The longest the retype check may run in one scan, across every service
/// (carrick#2021). No service has a limit of its own: its requests keep
/// going while they keep finishing, and the sidecar's 900 s with no sign of
/// life (`OPERATION_TIMEOUT`) stays the guard against a process that hangs.
///
/// Sized to the owner's target for a large monorepo's first index, 30 to 45
/// minutes in all (ruling, 2026-10-04). The rest of a large first index takes
/// about 30 of them: the one-service stand-in's (1,375 files) took 35 minutes
/// in all, of which its retype took about 3 on one process. That leaves 15
/// for the retype. On a pool of three processes the stand-in's retype judged
/// a call in 0.52 s, so 15 minutes judges about 1,700 calls, and about 800
/// on one process (carrick#1996).
const RETYPE_SCAN_CEILING: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// The scan's one retype ceiling ([`RETYPE_SCAN_CEILING`]), started when the
/// scan's retype starts and shared by every service after it.
struct RetypeCeiling {
    ends: std::time::Instant,
    length: std::time::Duration,
}

impl RetypeCeiling {
    fn starting_now(length: std::time::Duration) -> Self {
        Self {
            ends: std::time::Instant::now() + length,
            length,
        }
    }

    fn passed(&self) -> bool {
        std::time::Instant::now() >= self.ends
    }

    /// Why a call the ceiling cut off abstains. It opens as the sidecar's
    /// own budget sentence does, so the scan counts both as the retype time
    /// limit (`time_limits.rs`), and says the limit was the scan's.
    fn reason(&self) -> String {
        format!(
            "the retype check ran out of its {}ms budget for the whole scan",
            self.length.as_millis()
        )
    }
}

/// What a retype process is taken to need when its own size cannot be read:
/// a service's program and the checkers the retype builds over it.
const RETYPE_PROCESS_FLOOR_MB: u64 = 1024;

/// Why a call abstains when its request was never sent: the process it would
/// have gone to stopped answering, and no other process was left to take it.
const RETYPE_UNSENT: &str =
    "the retype check did not run: no sidecar process was left to send the call to";

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
    let ceiling = RetypeCeiling::starting_now(RETYPE_SCAN_CEILING);
    for (service, items) in by_service {
        let answers = retype_service(
            sidecar,
            service,
            &local_consumers[service],
            &items,
            &ceiling,
            |base| {
                crate::services::sidecar_pool::pool_size(
                    crate::services::sidecar_pool::resident_mb(base)
                        .unwrap_or(RETYPE_PROCESS_FLOOR_MB)
                        .max(RETYPE_PROCESS_FLOOR_MB),
                )
            },
        );
        for outcome in outcomes.iter_mut() {
            if let Some(answer) = answers.get(&outcome.pair_key) {
                apply_retype(outcome, answer);
            }
        }
    }
}

/// Retype one service's items, one request per file, until the scan's
/// retype `ceiling` passes (carrick#1945, carrick#1996, carrick#2021).
///
/// A request holds one file's calls, never part of a file: the process that
/// takes it reads that file's diagnostics before any rewrite once, for all
/// of them. One file a request also lets a pool share the work evenly, and a
/// failure costs only that file's calls.
///
/// Every file the calls are in first joins `sidecar`'s program, sorted, so
/// that no request adds one. The first request goes to `sidecar`, scoped to
/// the service: it builds the service's program, whose size then decides how
/// many processes the rest may use (`size`). The rest are answered by a pool
/// of that many processes, `sidecar` among them, each started from
/// `sidecar`'s program and each request on whichever is free next. A pool of
/// one is `sidecar` alone, taking them in order. Every item is judged by the
/// same code in the same program whichever process takes it.
///
/// A request that fails abstains its own items with the reason and costs no
/// other request's answers. A process that is gone takes no more requests;
/// a request no process was left to take is not sent, and its items abstain
/// with [`RETYPE_UNSENT`]. Once `ceiling` has passed no further request is
/// sent, and its items abstain with the ceiling's reason; a request already
/// sent finishes.
fn retype_service(
    sidecar: &TypeSidecar,
    service: &str,
    consumer: &LocalConsumer,
    items: &[RetypeItem],
    ceiling: &RetypeCeiling,
    size: impl Fn(&TypeSidecar) -> crate::services::sidecar_pool::PoolSize,
) -> HashMap<String, RetypeOutcome> {
    use crate::services::sidecar_pool::{SidecarPool, jobs_by_file};
    use crate::services::type_sidecar::SidecarError;

    let started = std::time::Instant::now();
    let jobs = jobs_by_file(items, |item| item.file_path.as_str(), 1);
    let spent = || ceiling.passed();
    let ask = |process: &TypeSidecar,
               job: &Vec<RetypeItem>|
     -> Result<Vec<RetypeOutcome>, SidecarError> {
        if spent() {
            return Ok(abstained(job, &ceiling.reason()));
        }
        process.retype_check(job).inspect_err(|e| {
            warn!(
                "Retyping {service}'s consumer calls failed for {} call(s) in {}: {e}",
                job.len(),
                job.first().map_or("", |item| item.file_path.as_str())
            );
        })
    };
    let gone = |answer: &Result<Vec<RetypeOutcome>, SidecarError>| matches!(answer, Err(e) if e.leaves_no_process());

    let Some((first, rest)) = jobs.split_first() else {
        return HashMap::new();
    };
    if spent() {
        // Nothing will be sent, so the service's program is not loaded or
        // built for it.
        let reason = ceiling.reason();
        return jobs
            .iter()
            .flat_map(|job| abstained(job, &reason))
            .map(|answer| (answer.item_id.clone(), answer))
            .collect();
    }
    if let Err(e) = scope_to(sidecar, consumer) {
        warn!("Retyping {service}'s consumer calls did not start: {e}");
        let reason = format!("the retype check did not run: {e}");
        return jobs
            .iter()
            .flat_map(|job| abstained(job, &reason))
            .map(|answer| (answer.item_id.clone(), answer))
            .collect();
    }
    // Every file the calls are in joins the program before the first
    // request, in one order (carrick#2027). Asked file by file, a process
    // loads a file its tsconfig does not list when it reaches it, so the
    // program a call is judged in would depend on which calls came first and
    // on which process took them; the compiler orders two types of the same
    // name by the program's file order. A pool's processes start from this
    // program ([`SidecarPool::scoped`]).
    let mut files: Vec<PathBuf> = items
        .iter()
        .map(|item| PathBuf::from(&item.file_path))
        .collect();
    files.sort();
    files.dedup();
    let alone = match sidecar.add_program_files(&files) {
        Ok(_) => false,
        Err(e) if e.leaves_no_process() => {
            warn!("Retyping {service}'s consumer calls did not start: {e}");
            let reason = format!("the retype check did not run: {e}");
            return jobs
                .iter()
                .flat_map(|job| abstained(job, &reason))
                .map(|answer| (answer.item_id.clone(), answer))
                .collect();
        }
        Err(e) => {
            warn!(
                "Retyping {service}'s consumer calls on the scan's own process alone: the files of the calls could not join its program first: {e}"
            );
            true
        }
    };
    let first_answer = ask(sidecar, first);
    let first_gone = gone(&first_answer);
    let mut answers: Vec<Option<Result<Vec<RetypeOutcome>, SidecarError>>> =
        Vec::with_capacity(jobs.len());
    answers.push(Some(first_answer));
    let mut ran_on = 1;
    if first_gone || rest.is_empty() || spent() || alone {
        // A process that is gone takes no more, as in a pool. With the time
        // spent nothing more is sent, so no process is started for it. A
        // program that could not take the calls' files first is not copied.
        answers.extend(
            rest.iter()
                .map(|job| (!first_gone).then(|| ask(sidecar, job))),
        );
    } else {
        let wanted = size(sidecar);
        let processes = wanted.processes.min(rest.len()).max(1);
        info!(
            "Retyping {service}'s consumer calls: {} call(s) in {} file(s); {processes} process(es) for the {} after the first ({})",
            items.len(),
            jobs.len(),
            rest.len(),
            wanted.why
        );
        match SidecarPool::scoped(
            sidecar,
            &consumer.root,
            consumer.tsconfig.as_deref(),
            processes,
        ) {
            Ok(pool) => {
                if pool.processes() < processes {
                    info!(
                        "Retyping {service}'s consumer calls in {} process(es): the others did not start",
                        pool.processes()
                    );
                }
                ran_on = pool.processes();
                answers.extend(pool.run(rest, gone, ask));
            }
            Err(e) => {
                warn!("Retyping {service}'s consumer calls stopped: {e}");
                answers.extend(rest.iter().map(|_| Some(Err(e.clone()))));
            }
        }
    }
    info!(
        "Retyped {service}'s consumer calls: {} call(s) in {} file(s) took {:.1}s on {ran_on} process(es)",
        items.len(),
        jobs.len(),
        started.elapsed().as_secs_f64()
    );

    jobs.iter()
        .zip(answers)
        .flat_map(|(job, answer)| match answer {
            Some(Ok(outcomes)) => outcomes,
            Some(Err(e)) => abstained(job, &format!("the retype check did not run: {e}")),
            None => abstained(job, RETYPE_UNSENT),
        })
        .map(|answer| (answer.item_id.clone(), answer))
        .collect()
}

/// Every item of `job` abstaining for `reason`.
fn abstained(job: &[RetypeItem], reason: &str) -> Vec<RetypeOutcome> {
    job.iter()
        .map(|item| RetypeOutcome {
            item_id: item.item_id.clone(),
            outcome: RetypeVerdict::Abstain,
            diagnostics: Vec::new(),
            reason: Some(reason.to_string()),
        })
        .collect()
}

/// Point the sidecar's project at the consumer service, unless it already is.
fn scope_to(
    sidecar: &TypeSidecar,
    consumer: &LocalConsumer,
) -> Result<(), crate::services::type_sidecar::SidecarError> {
    if sidecar.is_scoped_to(&consumer.root, consumer.tsconfig.as_deref()) {
        return Ok(());
    }
    scope_within(
        sidecar,
        consumer,
        crate::services::type_sidecar::ready_budget(),
    )
}

/// [`scope_to`], waiting at most `budget` for the sidecar to be ready.
fn scope_within(
    sidecar: &TypeSidecar,
    consumer: &LocalConsumer,
    budget: std::time::Duration,
) -> Result<(), crate::services::type_sidecar::SidecarError> {
    sidecar.start_init(&consumer.root, consumer.tsconfig.as_deref());
    sidecar.wait_ready(budget).map_err(|e| match e {
        // `wait_ready` reports a sidecar that was not ready in time as a
        // timeout, whose words are the operation deadline's (900 s with no
        // sign of life), and the scan counts that sentence as the deadline
        // (carrick#2021). This is the readiness budget, so it says so.
        crate::services::type_sidecar::SidecarError::Timeout => {
            crate::services::type_sidecar::SidecarError::NotReady(format!(
                "it was not ready within the {}s readiness budget",
                budget.as_secs()
            ))
        }
        other => other,
    })
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
            outcome.consumer_reads = answer.diagnostics.iter().map(|d| d.line).collect();
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
        // Not a break, so nothing rides `diagnostic` (a reason is read as a
        // mismatch); the places the published type fails are a note.
        RetypeVerdict::Wider => {
            let places: Vec<String> = answer
                .diagnostics
                .iter()
                .map(|d| format!("{}:{}: {}", outcome.consumer_file, d.line, d.message))
                .collect();
            outcome.bucket = VerdictBucket::ProducerWider;
            outcome.gate = Some("retype:consumer".to_string());
            outcome.diagnostic = None;
            outcome.resolved = true;
            outcome.unresolved_reason = None;
            outcome.notes.push(format!(
                "{PRODUCER_WIDER_NOTE}; against the declared type: {}",
                places.join("; ")
            ));
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
    let reason = outcome
        .unresolved_reason
        .as_deref()
        .or(outcome.diagnostic.as_deref())
        .unwrap_or("no reason recorded");
    // A held-back mismatch says what the check found after why it is not
    // reported (carrick#2053, and carrick#2054 for a response whose case the
    // call does not state); no other unverified pair's diagnostic is added.
    // On one line, as every pair's line is: a compiler's chain of "is not
    // assignable" lines would otherwise leave the rest of it out of a grep.
    let found = match outcome.diagnostic.as_deref() {
        Some(found)
            if matches!(
                outcome.gate.as_deref(),
                Some(SAME_SERVICE_GATE | UNSTATED_MODE_GATE)
            ) =>
        {
            let one_line = found.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("; what the check found: {one_line}")
        }
        _ => String::new(),
    };
    Some(format!(
        "Types not verified: {} {} {} ({}:{} in {} against {}): {reason}{found}",
        outcome.pseudo_method,
        outcome.identity,
        kind,
        outcome.consumer_file,
        outcome.consumer_line,
        outcome.consumer_service,
        outcome.producer_service,
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
        consumer_reads: Vec::new(),
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
            unwidened_definition: None,
            response_modes: None,
            stated_values: Default::default(),
            v1_state_before_demotion: None,
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
            scanner_build: None,
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

    /// A pool size of `n` processes, for [`retype_service`].
    fn processes(n: usize) -> impl Fn(&TypeSidecar) -> crate::services::sidecar_pool::PoolSize {
        move |_| crate::services::sidecar_pool::PoolSize {
            processes: n,
            why: format!("{n} process(es) for the test"),
        }
    }

    /// A pool size that must not be asked for: no pool is started.
    fn no_pool(_: &TypeSidecar) -> crate::services::sidecar_pool::PoolSize {
        panic!("no pool is started here")
    }

    /// The consumer service at `root`, for [`retype_service`].
    fn consumer_at(root: &std::path::Path) -> LocalConsumer {
        LocalConsumer {
            root: root.to_path_buf(),
            tsconfig: None,
            calls: HashMap::new(),
        }
    }

    /// Each answer's outcome and reason, by item id.
    fn read_answers(
        answers: &HashMap<String, RetypeOutcome>,
    ) -> BTreeMap<String, (RetypeVerdict, Option<String>)> {
        answers
            .iter()
            .map(|(id, answer)| (id.clone(), (answer.outcome, answer.reason.clone())))
            .collect()
    }

    /// carrick#1945, carrick#1996: a service's retype goes one file a
    /// request, and a request that fails costs only its own items. The
    /// stand-in fails the request that holds `fail-1`. The other files keep
    /// their answers, whether one process or three take them, and the
    /// failed file's two calls abstain and say why. A file is never split:
    /// `x-4` shares the failed request with `fail-1`.
    ///
    /// Sending every item in one request again fails this: all five come
    /// back from the one failed request. A spent budget sends nothing,
    /// starts no pool, and abstains every item in the words the sidecar
    /// uses for its own budget.
    #[test]
    fn a_retype_request_that_fails_costs_only_its_own_items() {
        use crate::services::sidecar_pool::test_support::{base_at, retype_item};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sidecar = base_at(&root);
        let consumer = consumer_at(&root);
        let file = |name: &str| root.join(name).display().to_string();
        let items = [
            ("src/a.ts", "x-0"),
            ("src/c.ts", "fail-1"),
            ("src/a.ts", "x-2"),
            ("src/c.ts", "x-4"),
            ("src/e.ts", "x-6"),
        ]
        .map(|(name, id)| retype_item(&file(name), id));

        for n in [1, 3] {
            let answers = read_answers(&retype_service(
                &sidecar,
                "web",
                &consumer,
                &items,
                &RetypeCeiling::starting_now(std::time::Duration::from_secs(600)),
                processes(n),
            ));
            assert_eq!(answers.len(), 5, "{n} process(es): {answers:?}");
            for id in ["x-0", "x-2", "x-6"] {
                assert_eq!(answers[id].0, RetypeVerdict::Agrees, "{n}: {id}");
            }
            for id in ["fail-1", "x-4"] {
                assert_eq!(
                    answers[id],
                    (
                        RetypeVerdict::Abstain,
                        Some(
                            "the retype check did not run: v2 check failed: the stand-in fails this request"
                                .to_string()
                        )
                    ),
                    "{n}: {id}"
                );
            }
        }

        let spent = retype_service(
            &sidecar,
            "web",
            &consumer,
            &items,
            &RetypeCeiling::starting_now(std::time::Duration::ZERO),
            no_pool,
        );
        assert_eq!(spent.len(), 5);
        for answer in spent.values() {
            assert_eq!(answer.outcome, RetypeVerdict::Abstain);
            assert_eq!(
                answer.reason.as_deref(),
                Some("the retype check ran out of its 0ms budget for the whole scan")
            );
        }
    }

    /// carrick#1996: the requests after the first are answered by a pool of
    /// processes, and every item gets the answer one process gives it. The
    /// stand-in judges an item by its id alone and names the process that
    /// answered, so the test can see that three did, the scan's own among
    /// them.
    #[test]
    fn a_pool_of_processes_gives_every_item_the_answer_one_process_gives() {
        use crate::services::sidecar_pool::test_support::{base_at, retype_item};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sidecar = base_at(&root);
        let consumer = consumer_at(&root);
        let items: Vec<RetypeItem> = (0..9)
            .map(|n| {
                retype_item(
                    &root.join(format!("src/f{n}.ts")).display().to_string(),
                    &format!("item-{n}"),
                )
            })
            .collect();
        let judge = |n: usize| {
            read_answers(&retype_service(
                &sidecar,
                "web",
                &consumer,
                &items,
                &RetypeCeiling::starting_now(std::time::Duration::from_secs(600)),
                processes(n),
            ))
        };
        let verdicts = |answers: &BTreeMap<String, (RetypeVerdict, Option<String>)>| {
            answers
                .iter()
                .map(|(id, (verdict, _))| (id.clone(), *verdict))
                .collect::<Vec<_>>()
        };
        let answered_by = |answers: &BTreeMap<String, (RetypeVerdict, Option<String>)>| {
            answers
                .values()
                .filter_map(|(_, pid)| pid.clone())
                .collect::<std::collections::BTreeSet<String>>()
        };

        let one = judge(1);
        let pooled = judge(3);
        assert_eq!(one.len(), 9);
        assert_eq!(verdicts(&pooled), verdicts(&one));
        assert_eq!(
            answered_by(&one),
            [sidecar.pid().to_string()].into(),
            "one process: the scan's own answers every request"
        );
        let pids = answered_by(&pooled);
        assert_eq!(pids.len(), 3, "three processes answered: {pooled:?}");
        assert!(pids.contains(&sidecar.pid().to_string()));
    }

    /// carrick#1996: a process that is gone takes no more requests, and a
    /// request no process was left to take is not sent: its calls abstain
    /// and say so. When the scan's own process dies on the first request,
    /// no pool is started for the rest. When every process of a pool dies,
    /// the requests nobody took are the unsent ones.
    #[test]
    fn a_request_no_process_was_left_to_take_abstains_unsent() {
        use crate::services::sidecar_pool::test_support::{base_at, retype_item};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let consumer = consumer_at(&root);
        let file = |name: &str| root.join(name).display().to_string();
        let died = |answer: &RetypeOutcome| {
            answer.outcome == RetypeVerdict::Abstain
                && answer.reason.as_deref().is_some_and(|reason| {
                    reason.starts_with("the retype check did not run: ") && reason != RETYPE_UNSENT
                })
        };
        let unsent = |answer: &RetypeOutcome| {
            answer.outcome == RetypeVerdict::Abstain
                && answer.reason.as_deref() == Some(RETYPE_UNSENT)
        };

        let first_dies = [
            ("src/a.ts", "die-0"),
            ("src/b.ts", "x-2"),
            ("src/c.ts", "x-4"),
        ]
        .map(|(name, id)| retype_item(&file(name), id));
        let answers = retype_service(
            &base_at(&root),
            "web",
            &consumer,
            &first_dies,
            &RetypeCeiling::starting_now(std::time::Duration::from_secs(600)),
            no_pool,
        );
        assert!(died(&answers["die-0"]), "{answers:?}");
        assert!(
            unsent(&answers["x-2"]) && unsent(&answers["x-4"]),
            "{answers:?}"
        );

        // The first request is answered; the pool's three processes each
        // take one of the next three and die on it; nobody takes the last.
        let all_die = [
            ("src/a.ts", "x-0"),
            ("src/b.ts", "die-1"),
            ("src/c.ts", "die-3"),
            ("src/d.ts", "die-5"),
            ("src/e.ts", "x-6"),
        ]
        .map(|(name, id)| retype_item(&file(name), id));
        let answers = retype_service(
            &base_at(&root),
            "web",
            &consumer,
            &all_die,
            &RetypeCeiling::starting_now(std::time::Duration::from_secs(600)),
            processes(3),
        );
        assert_eq!(answers["x-0"].outcome, RetypeVerdict::Agrees, "{answers:?}");
        for id in ["die-1", "die-3", "die-5"] {
            assert!(died(&answers[id]), "{id}: {answers:?}");
        }
        assert!(unsent(&answers["x-6"]), "{answers:?}");
    }

    /// carrick#1996, carrick#2027: every call is judged in one program that
    /// holds the file of every call, sorted, whichever process takes it and
    /// however many there are. The files join the scan process's program
    /// before the first request, and a pool's processes start from that
    /// program. Asked file by file, a process would load each file when it
    /// reached it, in the order of the calls.
    #[test]
    fn every_call_is_judged_in_one_program_holding_every_file_asked_about() {
        use crate::services::sidecar_pool::test_support::{base_at, retype_item};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sidecar = base_at(&root);
        let consumer = consumer_at(&root);
        let file = |name: &str| root.join(name).display().to_string();
        let names = ["src/d.ts", "src/b.ts", "src/a.ts", "src/c.ts", "src/e.ts"];
        let items: Vec<RetypeItem> = names
            .iter()
            .enumerate()
            .map(|(n, name)| retype_item(&file(name), &format!("x-{n}")))
            .collect();
        let mut sorted: Vec<String> = names.iter().map(|name| file(name)).collect();
        sorted.sort();
        let program = sorted.join(",");

        for n in [1, 3] {
            let answers = retype_service(
                &sidecar,
                "web",
                &consumer,
                &items,
                &RetypeCeiling::starting_now(std::time::Duration::from_secs(600)),
                processes(n),
            );
            assert_eq!(answers.len(), 5);
            for answer in answers.values() {
                assert_eq!(
                    answer.diagnostics[0].message, program,
                    "{n} process(es): {} was judged in another program",
                    answer.item_id
                );
            }
        }
    }

    /// A pair outcome of `consumer`'s, unresolved, carrying `answer` as the
    /// retype writes it, for what the scan counts against its time limits.
    fn retyped_outcome(consumer: &str, answer: &RetypeOutcome) -> PairCheckOutcome {
        let mut outcome = PairCheckOutcome {
            pair_key: answer.item_id.clone(),
            pseudo_method: "GET".to_string(),
            identity: "/orders".to_string(),
            consumer_file: "src/client.ts".to_string(),
            consumer_line: 1,
            type_kind: ManifestTypeKind::Response,
            bucket: VerdictBucket::Unverifiable,
            gate: None,
            diagnostic: None,
            producer_alias: "Res".to_string(),
            consumer_alias: "Call".to_string(),
            producer_service: "api".to_string(),
            consumer_service: consumer.to_string(),
            resolved: false,
            unresolved_reason: None,
            notes: Vec::new(),
            consumer_reads: Vec::new(),
        };
        apply_retype(&mut outcome, answer);
        outcome
    }

    /// carrick#2021: the retype has one ceiling for the whole scan, and no
    /// limit of its own for a service. A service's requests keep going while
    /// they keep finishing: thirty files, one request each, are all judged.
    /// The ceiling is shared: once one service has spent it, the next
    /// service sends nothing and is not even loaded, and every call it cut
    /// off abstains with the ceiling's reason and is counted as the retype
    /// time limit.
    ///
    /// The stand-in spends at least 150 ms on a request, so thirty requests
    /// spend at least 4.5 s and a 1.5 s ceiling passes inside the first
    /// service whatever the machine's load.
    #[test]
    fn the_retype_has_one_ceiling_for_the_whole_scan_and_none_for_a_service() {
        use crate::services::sidecar_pool::test_support::{base_at, retype_item};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sidecar = base_at(&root);
        let consumer = consumer_at(&root);
        let calls = |prefix: &str| -> Vec<RetypeItem> {
            (0..30)
                .map(|n| {
                    retype_item(
                        &root
                            .join(format!("src/{prefix}{n}.ts"))
                            .display()
                            .to_string(),
                        &format!("{prefix}-{n}0"),
                    )
                })
                .collect()
        };
        let judged = |answers: &HashMap<String, RetypeOutcome>| {
            answers
                .values()
                .filter(|answer| answer.outcome == RetypeVerdict::Agrees)
                .count()
        };

        let unhurried = RetypeCeiling::starting_now(std::time::Duration::from_secs(600));
        let whole = retype_service(
            &sidecar,
            "web",
            &consumer,
            &calls("a"),
            &unhurried,
            processes(1),
        );
        assert_eq!(
            judged(&whole),
            30,
            "every call of a service that keeps finishing is judged"
        );

        let ceiling = RetypeCeiling::starting_now(std::time::Duration::from_millis(1500));
        let first = retype_service(
            &sidecar,
            "web",
            &consumer,
            &calls("b"),
            &ceiling,
            processes(1),
        );
        let admin = consumer_at(&root.join("admin"));
        let after = retype_service(
            &sidecar,
            "admin",
            &admin,
            &calls("c"),
            &ceiling,
            processes(1),
        );
        assert!(
            sidecar.is_scoped_to(&root, None),
            "a service the ceiling cut off entirely is not loaded"
        );
        assert!(
            (1..30).contains(&judged(&first)),
            "the ceiling passed inside the first service: {} judged",
            judged(&first)
        );
        assert_eq!(
            judged(&after),
            0,
            "the next service found the ceiling spent"
        );
        let cut_off = "the retype check ran out of its 1500ms budget for the whole scan";
        for answer in first.values().chain(after.values()) {
            assert!(
                answer.outcome == RetypeVerdict::Agrees
                    || answer.reason.as_deref() == Some(cut_off),
                "{answer:?}"
            );
        }

        let outcomes: Vec<PairCheckOutcome> = first
            .values()
            .map(|answer| retyped_outcome("web", answer))
            .chain(
                after
                    .values()
                    .map(|answer| retyped_outcome("admin", answer)),
            )
            .collect();
        let counted = crate::time_limits::from_check_outcomes(&outcomes);
        assert_eq!(counted["web"].retype, 30 - judged(&first));
        assert_eq!(counted["admin"].retype, 30);
    }

    /// carrick#2021: a consumer's sidecar that is not ready within the
    /// readiness budget is said to be not ready, not silent for the
    /// operation deadline, and the scan does not count it as that deadline.
    #[test]
    fn a_sidecar_not_ready_in_time_is_not_counted_as_the_operation_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let script = root.join("silent-sidecar.cjs");
        std::fs::write(
            &script,
            "require('readline').createInterface({ input: process.stdin, terminal: false }).on('line', () => {});\n",
        )
        .unwrap();
        let sidecar = TypeSidecar::spawn(&script).unwrap();
        let error = scope_within(
            &sidecar,
            &consumer_at(&root),
            std::time::Duration::from_secs(1),
        )
        .expect_err("the stand-in never answers init");
        let reason = format!("the retype check did not run: {error}");
        assert_eq!(
            reason,
            "the retype check did not run: Sidecar not ready: it was not ready within the 1s readiness budget"
        );
        let answer = RetypeOutcome {
            item_id: "web/Call".to_string(),
            outcome: RetypeVerdict::Abstain,
            diagnostics: Vec::new(),
            reason: Some(reason),
        };
        let counted = crate::time_limits::from_check_outcomes(&[retyped_outcome("web", &answer)]);
        assert!(counted.is_empty(), "{counted:?}");
    }

    /// carrick#1516: the producer's unwidened reading rides the retype item
    /// beside its published type, and is dropped on the same terms.
    #[test]
    fn retype_items_carry_the_unwidened_reading() {
        let local = web_consumer();
        let mut pair = retype_pair(ManifestTypeKind::Response, Some("{ scope: string; }"));
        pair.producer_unwidened = Some("{ scope: \"all\" | \"specific\"; }".to_string());
        let blamed = HashSet::from([pair.spec.pair_key.clone()]);
        let pairs = [pair];
        let items = retype_items(&pairs, &blamed, &local);
        assert_eq!(
            items["web"][0].producer_unwidened_type.as_deref(),
            Some("{ scope: \"all\" | \"specific\"; }")
        );

        let mut loose = retype_pair(ManifestTypeKind::Response, Some("{ scope: string; }"));
        loose.producer_unwidened = Some("{ scope: \"all\"; meta: any; }".to_string());
        let blamed = HashSet::from([loose.spec.pair_key.clone()]);
        let loose = [loose];
        let items = retype_items(&loose, &blamed, &local);
        assert_eq!(
            items["web"][0].producer_unwidened_type, None,
            "a reading with a top type in it could agree with anything"
        );
    }

    /// carrick#1516: an inference whose handler returns a literal TypeScript
    /// widened is captured twice: the published type under its own alias, the
    /// unwidened reading under the sibling alias. Equal texts, or a reading
    /// with a top type in it, add nothing.
    #[test]
    fn the_unwidened_reading_is_captured_beside_the_published_type() {
        let request = InferRequestItem {
            file_path: "/repo/src/routes.ts".to_string(),
            line_number: 7,
            span_start: None,
            span_end: None,
            expression_text: None,
            expression_line: None,
            infer_kind: InferKind::ResponseBody,
            alias: Some("Endpoint_a_Response".to_string()),
            param_name: None,
        };
        let with = |unwidened: Option<&str>| {
            let mut inf = inferred("Endpoint_a_Response", "{ scope: string; }", None, None);
            inf.unwidened_type_string = unwidened.map(str::to_string);
            derive_capture_anchors(
                &[],
                std::slice::from_ref(&request),
                &[],
                &[inf],
                &["Endpoint_a_Response".to_string()],
                "/repo",
            )
        };
        let literal = |anchor: &CaptureAnchor| match anchor {
            CaptureAnchor::Literal {
                alias, type_text, ..
            } => (alias.clone(), type_text.clone()),
            other => panic!("expected a literal anchor, got {other:?}"),
        };

        let anchors = with(Some("{ scope: \"all\" | \"specific\"; }"));
        assert_eq!(
            anchors.iter().map(literal).collect::<Vec<_>>(),
            vec![
                (
                    "Endpoint_a_Response".to_string(),
                    "{ scope: string; }".to_string()
                ),
                (
                    "Endpoint_a_Response_Unwidened".to_string(),
                    "{ scope: \"all\" | \"specific\"; }".to_string()
                ),
            ]
        );
        assert_eq!(with(None).len(), 1);
        assert_eq!(with(Some("{ scope: string; }")).len(), 1);
        assert_eq!(with(Some("{ scope: any; }")).len(), 1);
    }

    /// carrick#1842: a body the call site reads as raw text marks the literal
    /// anchor that publishes the inference's text, and no other anchor.
    #[test]
    fn a_raw_text_read_marks_the_literal_anchor_that_publishes_it() {
        let request = |alias: &str| InferRequestItem {
            file_path: "/repo/src/client.ts".to_string(),
            line_number: 9,
            span_start: None,
            span_end: None,
            expression_text: None,
            expression_line: None,
            infer_kind: InferKind::CallResult,
            alias: Some(alias.to_string()),
            param_name: None,
        };
        let mut text_read = inferred("Endpoint_a_Response_Call1", "string", None, None);
        text_read.infer_kind = InferKind::CallResult;
        text_read.raw_text_read = true;
        let mut json_string = inferred("Endpoint_b_Response_Call2", "string", None, None);
        json_string.infer_kind = InferKind::CallResult;
        let anchors = derive_capture_anchors(
            &[],
            &[
                request("Endpoint_a_Response_Call1"),
                request("Endpoint_b_Response_Call2"),
            ],
            &[],
            &[text_read, json_string],
            &[
                "Endpoint_a_Response_Call1".to_string(),
                "Endpoint_b_Response_Call2".to_string(),
                "Endpoint_c_Response_Call3".to_string(),
            ],
            "/repo",
        );
        let marks: Vec<(&str, bool)> = anchors
            .iter()
            .map(|anchor| match anchor {
                CaptureAnchor::Literal {
                    alias,
                    raw_text_read,
                    ..
                } => (alias.as_str(), *raw_text_read),
                other => panic!("expected a literal anchor, got {other:?}"),
            })
            .collect();
        assert_eq!(
            marks,
            vec![
                ("Endpoint_a_Response_Call1", true),
                ("Endpoint_b_Response_Call2", false),
                ("Endpoint_c_Response_Call3", false),
            ]
        );
        let wire = serde_json::to_value(&anchors[0]).unwrap();
        assert_eq!(wire["raw_text_read"], serde_json::json!(true));
        let unmarked = serde_json::to_value(&anchors[1]).unwrap();
        assert!(unmarked.get("raw_text_read").is_none(), "{unmarked}");
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
        // It reads bytes (carrick#1793): a blob or a buffer states no shape,
        // so retyping its call with the producer's type has nothing to judge.
        assert!(!consumer_to_blame(&verdict(
            consumer,
            Some("consumer:bytes")
        )));
        // It reads the body as raw text (carrick#1842): retyping its call with
        // the producer's type would judge a text read against a JSON shape.
        assert!(!consumer_to_blame(&verdict(
            consumer,
            Some("consumer:text")
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
        // carrick#1517: the finding names these reads as the consumer's side.
        assert_eq!(mismatch.consumer_reads, vec![9]);

        let mut agrees = unresolved();
        apply_retype(&mut agrees, &answer(RetypeVerdict::Agrees, None));
        assert_eq!(agrees.bucket, VerdictBucket::Compatible);
        assert!(agrees.resolved);
        assert_eq!((agrees.gate, agrees.diagnostic), (None, None));

        // carrick#1516: its own class, a fact, and never a mismatch reason:
        // what the declared type fails on is a note.
        let mut wider = unresolved();
        apply_retype(&mut wider, &answer(RetypeVerdict::Wider, None));
        assert_eq!(wider.bucket, VerdictBucket::ProducerWider);
        assert!(wider.resolved);
        assert_eq!(wider.unresolved_reason, None);
        assert_eq!(wider.diagnostic, None);
        assert_eq!(
            wider.notes,
            vec![
                format!(
                    "{PRODUCER_WIDER_NOTE}; against the declared type: src/client.ts:9: \
                     Property 'x' does not exist on type '{{ y: number; }}'."
                ),
                RETYPE_NOTE.to_string(),
            ]
        );

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
            consumer_response: false,
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
            unwidened_type_string: None,
            stated_body: None,
            printed_names: Vec::new(),
            raw_text_read: false,
            response_modes: None,
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
            consumer_response: false,
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

    /// carrick#1841: a symbol requested for a consumer's response carries
    /// that fact onto its capture anchor, where the sidecar reads a response
    /// table keyed by status code as its 2xx body. Any other symbol anchor
    /// does not carry it.
    #[test]
    fn a_consumer_response_symbol_keeps_its_marker_on_the_capture_anchor() {
        let explicit = vec![
            SymbolRequest {
                consumer_response: true,
                ..order_explicit("Endpoint_consumer_Response")
            },
            order_explicit("Endpoint_producer_Response"),
        ];

        let anchors = derive_capture_anchors(&explicit, &[], &[], &[], &[], "/repo");

        let markers: Vec<(&str, bool)> = anchors
            .iter()
            .map(|anchor| match anchor {
                CaptureAnchor::Symbol {
                    alias,
                    consumer_response,
                    ..
                } => (alias.as_str(), *consumer_response),
                other => panic!("expected symbol anchors, got {other:?}"),
            })
            .collect();
        assert_eq!(
            markers,
            vec![
                ("Endpoint_consumer_Response", true),
                ("Endpoint_producer_Response", false),
            ]
        );
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
                    ..
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

    /// carrick#1836: the sidecar's printer writes some names bare, and only the
    /// inference knows what they meant. Both texts it published reach the
    /// capture with that record, or the capture reads the names nowhere.
    #[test]
    fn derive_anchors_hand_the_printed_names_to_both_literal_texts() {
        let infer = vec![response_body_infer("Endpoint_row_Response")];
        let names = vec![crate::services::type_sidecar::PrintedName {
            name: "Status".to_string(),
            file: "/repo/src/db/enums.ts".to_string(),
            export_path: vec!["Status".to_string()],
        }];
        let mut inference = inferred(
            "Endpoint_row_Response",
            "{ status: Status; scope: string; }",
            None,
            None,
        );
        inference.unwidened_type_string = Some("{ status: Status; scope: \"all\"; }".to_string());
        inference.printed_names = names.clone();

        let anchors = derive_capture_anchors(&[], &infer, &[], &[inference], &[], "/repo");

        let printed: Vec<(&str, &[crate::services::type_sidecar::PrintedName])> = anchors
            .iter()
            .map(|anchor| match anchor {
                CaptureAnchor::Literal {
                    alias,
                    printed_names,
                    ..
                } => (alias.as_str(), printed_names.as_slice()),
                other => panic!("expected the inference's literal texts, got {other:?}"),
            })
            .collect();
        assert_eq!(
            printed,
            vec![
                ("Endpoint_row_Response", names.as_slice()),
                ("Endpoint_row_Response_Unwidened", names.as_slice()),
            ]
        );
    }

    fn query_mode() -> crate::services::type_sidecar::MessageRead {
        crate::services::type_sidecar::MessageRead {
            location: crate::services::type_sidecar::MessageSource::Query,
            field: "mode".to_string(),
        }
    }

    fn modes_inference(alias: &str) -> crate::services::type_sidecar::InferredType {
        use crate::services::type_sidecar::{InferredModeCase, InferredResponseModes};
        let mut inference = inferred(alias, "{ x: number; } | { y: string; }", None, None);
        inference.response_modes = Some(InferredResponseModes {
            reads: vec![query_mode()],
            cases: vec![
                InferredModeCase {
                    value: Some("a".to_string()),
                    type_string: "{ x: number; }".to_string(),
                },
                InferredModeCase {
                    value: None,
                    type_string: "{ y: string; }".to_string(),
                },
            ],
        });
        inference
    }

    /// carrick#2054: each case of a response's modes is captured beside the
    /// alias as a literal of its own, after the published anchor, sorted, and
    /// with the inference's printed names; a symbol anchor that publishes the
    /// alias does not stop them. A union with reads and no cases, or no
    /// modes at all, captures exactly what main did.
    #[test]
    fn derive_anchors_capture_each_case_of_a_response_s_modes() {
        let alias = "Endpoint_row_Response";
        let infer = vec![response_body_infer(alias)];
        let case_a = mode_alias(alias, &query_mode(), Some("a"));
        let other = mode_alias(alias, &query_mode(), None);
        let literals = |anchors: &[CaptureAnchor]| -> Vec<(String, String)> {
            anchors
                .iter()
                .map(|anchor| match anchor {
                    CaptureAnchor::Literal {
                        alias, type_text, ..
                    } => (alias.clone(), type_text.clone()),
                    CaptureAnchor::Symbol { alias, .. } => (alias.clone(), "symbol".to_string()),
                    other => panic!("unexpected anchor {other:?}"),
                })
                .collect()
        };
        let mut expected = vec![
            (case_a.clone(), "{ x: number; }".to_string()),
            (other.clone(), "{ y: string; }".to_string()),
        ];
        expected.sort();

        let anchors =
            derive_capture_anchors(&[], &infer, &[], &[modes_inference(alias)], &[], "/repo");
        let mut want = vec![(
            alias.to_string(),
            "{ x: number; } | { y: string; }".to_string(),
        )];
        want.extend(expected.clone());
        assert_eq!(literals(&anchors), want);
        assert!(
            anchors
                .iter()
                .all(|anchor| anchor.source_file() == Some("src/handler.ts")),
            "{anchors:?}"
        );

        let symbol = order_explicit(alias);
        let mut sighted = modes_inference(alias);
        sighted.primary_type_symbol = Some("Order".to_string());
        let anchors = derive_capture_anchors(&[symbol], &infer, &[], &[sighted], &[], "/repo");
        let mut want = vec![(alias.to_string(), "symbol".to_string())];
        want.extend(expected);
        assert_eq!(literals(&anchors), want);

        let mut reads_only = modes_inference(alias);
        reads_only.response_modes.as_mut().unwrap().cases.clear();
        let plain = inferred(alias, "{ x: number; } | { y: string; }", None, None);
        for inference in [reads_only, plain] {
            let anchors = derive_capture_anchors(&[], &infer, &[], &[inference], &[], "/repo");
            assert_eq!(
                literals(&anchors),
                vec![(
                    alias.to_string(),
                    "{ x: number; } | { y: string; }".to_string()
                )]
            );
        }
    }

    fn moded_modes(cases: &[(Option<&str>, Option<&str>)]) -> crate::cloud_storage::ResponseModes {
        crate::cloud_storage::ResponseModes {
            reads: vec![query_mode()],
            cases: cases
                .iter()
                .map(|(value, expanded)| crate::cloud_storage::ResponseModeCase {
                    value: value.map(str::to_string),
                    alias: mode_alias("P", &query_mode(), *value),
                    expanded: expanded.map(str::to_string),
                })
                .collect(),
        }
    }

    fn stated(
        source: crate::services::type_sidecar::MessageSource,
        field: &str,
        value: &str,
    ) -> crate::cloud_storage::StatedValues {
        crate::cloud_storage::StatedValues::from([(
            source,
            std::collections::BTreeMap::from([(field.to_string(), value.to_string())]),
        )])
    }

    /// carrick#2054: which case a call's stated message picks.
    #[test]
    fn select_mode_picks_the_case_the_call_states() {
        use crate::services::type_sidecar::{MessageRead, MessageSource};
        let modes = moded_modes(&[
            (Some("a"), Some("{ x: number; }")),
            (Some("b"), None),
            (None, Some("{ y: string; }")),
        ]);
        let query = |value: &str| stated(MessageSource::Query, "mode", value);
        let none = crate::cloud_storage::StatedValues::new();
        let placed = "Not compared: this route returns a different response for each value of `mode` in the request (`a`, `b`), and this call does not set `mode` to one of them.";
        let unplaced = "Not compared: this route returns a different response depending on `mode` in the request, and Carrick cannot tell which one this call receives.";

        assert_eq!(
            select_mode(Some(&modes), &query("a")),
            ModeSelection::Narrowed(&modes.cases[0], "{ x: number; }")
        );
        assert_eq!(
            select_mode(Some(&modes), &query("z")),
            ModeSelection::Narrowed(&modes.cases[2], "{ y: string; }"),
            "a value no case names takes the case for any other value"
        );
        assert_eq!(
            select_mode(Some(&modes), &none),
            ModeSelection::Unstated(placed.to_string())
        );
        assert_eq!(
            select_mode(Some(&modes), &stated(MessageSource::Body, "mode", "a")),
            ModeSelection::Unstated(placed.to_string()),
            "a value stated in the other location is not stated"
        );
        assert_eq!(
            select_mode(Some(&modes), &query("b")),
            ModeSelection::Unstated(unplaced.to_string()),
            "a case the capture could not publish narrows nothing"
        );
        let no_other = moded_modes(&[(Some("a"), Some("{ x: number; }"))]);
        assert_eq!(
            select_mode(Some(&no_other), &query("z")),
            ModeSelection::Unstated(
                "Not compared: this route returns a different response for each value of `mode` in the request (`a`), and this call does not set `mode` to one of them.".to_string()
            ),
            "with no case for any other value, an unnamed value is unstated"
        );

        let mut reads_only = modes.clone();
        reads_only.cases.clear();
        assert_eq!(
            select_mode(Some(&reads_only), &query("a")),
            ModeSelection::Unstated(unplaced.to_string())
        );
        let mut unplaced_read = modes.clone();
        unplaced_read.reads[0].location = MessageSource::Unplaced;
        assert_eq!(
            select_mode(Some(&unplaced_read), &query("a")),
            ModeSelection::Unstated(unplaced.to_string())
        );
        let mut two_fields = modes.clone();
        two_fields.reads.push(MessageRead {
            location: MessageSource::Body,
            field: "kind".to_string(),
        });
        assert_eq!(
            select_mode(Some(&two_fields), &query("a")),
            ModeSelection::Unstated(
                "Not compared: this route returns a different response depending on `kind` and `mode` in the request, and Carrick cannot tell which one this call receives.".to_string()
            )
        );

        assert_eq!(select_mode(None, &query("a")), ModeSelection::NoModes);
        let mut no_reads = modes.clone();
        no_reads.reads.clear();
        assert_eq!(
            select_mode(Some(&no_reads), &query("a")),
            ModeSelection::NoModes
        );
    }

    /// carrick#2054: a narrowed pair probes the case's alias and states the
    /// case's text to the retype, with no unwidened reading; its key, its
    /// producer alias and so its verdict key stay the published alias's. A
    /// pair with no modes is built as before, and a request pair is never
    /// narrowed.
    #[test]
    fn build_pair_narrows_a_moded_response_to_the_stated_case() {
        use crate::services::type_sidecar::MessageSource;
        let key = OperationKey::http("GET", "/items");
        let pair_with =
            |kind: ManifestTypeKind,
             modes: Option<crate::cloud_storage::ResponseModes>,
             stated_values: crate::cloud_storage::StatedValues| {
                let mut producer = entry(
                    key.clone(),
                    ManifestRole::Producer,
                    kind,
                    "P",
                    "src/routes.ts",
                    3,
                    ManifestTypeState::Implicit,
                );
                producer.expanded_definition = Some("{ x: number; } | { y: string; }".to_string());
                producer.unwidened_definition = Some("{ x: 1; } | { y: string; }".to_string());
                producer.response_modes = modes;
                let mut consumer = entry(
                    key.clone(),
                    ManifestRole::Consumer,
                    kind,
                    "C",
                    "src/client.ts",
                    8,
                    ManifestTypeState::Unknown,
                );
                consumer.stated_values = stated_values;
                let mut pairs = build_check_pairs(&[
                    repo("api", None, vec![producer], Some(fake_artifact())),
                    repo("web", None, vec![consumer], Some(fake_artifact())),
                ]);
                assert_eq!(pairs.len(), 1);
                pairs.remove(0)
            };
        let modes = moded_modes(&[
            (Some("a"), Some("{ x: number; }")),
            (None, Some("{ y: string; }")),
        ]);
        let a = stated(MessageSource::Query, "mode", "a");

        let narrowed = pair_with(ManifestTypeKind::Response, Some(modes.clone()), a.clone());
        assert_eq!(narrowed.spec.producer.alias, modes.cases[0].alias);
        assert_eq!(
            narrowed.producer_expanded.as_deref(),
            Some("{ x: number; }")
        );
        assert_eq!(narrowed.producer_unwidened, None);
        assert_eq!(narrowed.spec.pair_key, "api/P~web/C");
        assert_eq!(narrowed.producer_alias, "P");
        assert_eq!(narrowed.unstated_mode, None);

        let unstated = pair_with(
            ManifestTypeKind::Response,
            Some(modes.clone()),
            Default::default(),
        );
        assert_eq!(unstated.spec.producer.alias, "P");
        assert_eq!(
            unstated.producer_expanded.as_deref(),
            Some("{ x: number; } | { y: string; }")
        );
        assert!(unstated.unstated_mode.is_some());

        let plain = pair_with(ManifestTypeKind::Response, None, a.clone());
        assert_eq!(plain.spec.producer.alias, "P");
        assert_eq!(
            plain.producer_unwidened.as_deref(),
            Some("{ x: 1; } | { y: string; }")
        );
        assert_eq!(plain.unstated_mode, None);

        let request = pair_with(ManifestTypeKind::Request, Some(modes), a);
        assert_eq!(request.spec.producer.alias, "P");
        assert_eq!(request.unstated_mode, None);
    }

    /// carrick#2054: a mismatch against a response whose case the call does
    /// not state is published unverifiable with the pair's reason, before the
    /// same-service hold-back reads it. Every other outcome is untouched.
    #[test]
    fn an_unstated_mode_mismatch_is_published_unverifiable_with_its_reason() {
        let key = OperationKey::http("GET", "/items");
        let mut producer = entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            "P",
            "src/routes.ts",
            3,
            ManifestTypeState::Implicit,
        );
        producer.response_modes = Some(moded_modes(&[(Some("a"), Some("{ x: number; }"))]));
        let consumer = |alias: &str, line: u32| {
            entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                alias,
                "src/client.ts",
                line,
                ManifestTypeState::Unknown,
            )
        };
        let pairs = build_check_pairs(&[
            repo(
                "app",
                None,
                vec![producer, consumer("C1", 8), consumer("C2", 9)],
                Some(fake_artifact()),
            ),
            repo("web", None, vec![consumer("C3", 10)], Some(fake_artifact())),
        ]);
        assert_eq!(pairs.len(), 3);
        let reason = pairs[0].unstated_mode.clone().expect("unstated");
        let outcome = |pair: &BuiltPair, bucket: VerdictBucket| {
            let mut outcome = outcome_for(
                pair,
                bucket,
                None,
                Some("Property 'x' does not exist".to_string()),
                true,
                None,
                vec!["a note".to_string()],
            );
            outcome.consumer_reads = vec![14];
            outcome
        };
        let mut outcomes = vec![
            outcome(&pairs[0], VerdictBucket::Incompatible),
            outcome(&pairs[1], VerdictBucket::Compatible),
            outcome(&pairs[2], VerdictBucket::Incompatible),
        ];
        let compatible = format!("{:?}", outcomes[1]);
        abstain_on_unstated_modes(&pairs, &mut outcomes);
        hold_back_same_service_mismatches(&mut outcomes);

        for held in [&outcomes[0], &outcomes[2]] {
            assert_eq!(held.bucket, VerdictBucket::Unverifiable, "{held:?}");
            assert_eq!(held.gate.as_deref(), Some("producer:mode"), "{held:?}");
            assert_eq!(held.unresolved_reason.as_deref(), Some(reason.as_str()));
            assert!(!held.resolved);
            assert_eq!(
                held.diagnostic.as_deref(),
                Some("Property 'x' does not exist")
            );
            assert!(held.notes.is_empty() && held.consumer_reads.is_empty());
        }
        assert_eq!(
            format!("{:?}", outcomes[1]),
            compatible,
            "a compatible pair is kept"
        );
        assert!(
            unresolved_pair_line(&outcomes[2])
                .unwrap()
                .ends_with(&format!(
                    "{reason}; what the check found: Property 'x' does not exist"
                )),
            "the run log says what the check found"
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

    /// carrick#1749: the capture anchors what the arbitration left. A consumer
    /// call whose model symbol names the element of the body the source
    /// states (`res.json() as Promise<{ members: Member[] }>`, model `Member`)
    /// must reach the surface as the stated body, not as `Member`: once
    /// `demote_witnessed_borrowed_anchors` drops the symbol, the alias falls to
    /// its inference's literal text. Run in the order `run_capture_for_service`
    /// runs them.
    #[test]
    fn derive_anchors_capture_the_stated_body_over_a_model_element_symbol() {
        let alias = "Endpoint_members_Response_Call1";
        let explicit = vec![SymbolRequest {
            symbol_name: "Member".to_string(),
            source_file: "/repo/src/api.ts".to_string(),
            alias: Some(alias.to_string()),
            array_depth: None,
            payload_borrow_witness: false,
            consumer_response: false,
        }];
        let infer = vec![InferRequestItem {
            file_path: "/repo/src/api.ts".to_string(),
            line_number: 30,
            span_start: None,
            span_end: None,
            expression_text: Some("fetchReply(\"/v1/members\")".to_string()),
            expression_line: Some(30),
            infer_kind: InferKind::CallResult,
            alias: Some(alias.to_string()),
            param_name: None,
        }];
        let text = "{ members: { id: string; name: string; email: string; }[]; }";
        let mut inf = inferred(alias, text, None, None);
        inf.infer_kind = InferKind::CallResult;
        inf.is_explicit = true;
        inf.stated_body = Some(crate::services::type_sidecar::StatedBody::default());
        let inferred_types = vec![inf];

        let explicit = crate::services::type_sidecar::demote_witnessed_borrowed_anchors(
            &explicit,
            &inferred_types,
        )
        .requests;
        let explicit =
            crate::services::type_sidecar::apply_inferred_array_depth(&explicit, &inferred_types);
        let anchors = derive_capture_anchors(&explicit, &infer, &[], &inferred_types, &[], "/repo");

        assert_eq!(anchors.len(), 1, "{anchors:?}");
        match &anchors[0] {
            CaptureAnchor::Literal {
                alias: anchored,
                type_text,
                anchor_origin,
                ..
            } => {
                assert_eq!(anchored, alias);
                assert_eq!(type_text, text);
                assert_eq!(*anchor_origin, AnchorOrigin::DeterministicInfer);
            }
            other => panic!("expected the stated body as a literal anchor, got {other:?}"),
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

    /// carrick#1841: a consumer call whose result carries a library's own
    /// response object is answered `unknown` with `machinery_envelope` at the
    /// root. That is a decision: the capture's raw locator re-run would resolve
    /// the call and publish the carrier around the response object. A handler
    /// return with the same reason (carrick#166, some union branches unread)
    /// keeps its infer anchor, as before.
    #[test]
    fn derive_anchors_keeps_a_call_result_decided_as_transport() {
        let machinery = || crate::services::type_sidecar::TypeProvenance {
            path: String::new(),
            kind: "unknown".to_string(),
            reason: "machinery_envelope".to_string(),
            detail: None,
        };
        let mut call = inferred("Endpoint_ping_Response_Call1", "unknown", None, None);
        call.infer_kind = InferKind::CallResult;
        call.any_provenance = vec![machinery()];
        let mut handler = inferred("Endpoint_partial_Response", "unknown", None, None);
        handler.any_provenance = vec![machinery()];
        let mut call_request = response_body_infer("Endpoint_ping_Response_Call1");
        call_request.infer_kind = InferKind::CallResult;
        let infer = vec![
            call_request,
            response_body_infer("Endpoint_partial_Response"),
        ];

        let anchors = derive_capture_anchors(&[], &infer, &[], &[call, handler], &[], "/repo");

        assert_eq!(anchors.len(), 2, "{anchors:?}");
        assert!(
            matches!(
                &anchors[0],
                CaptureAnchor::Literal { alias, type_text, .. }
                    if alias == "Endpoint_ping_Response_Call1" && type_text == "unknown"
            ),
            "a call result decided as transport must stay a literal unknown, got {:?}",
            anchors[0]
        );
        assert!(
            matches!(&anchors[1], CaptureAnchor::Infer { alias, .. } if alias == "Endpoint_partial_Response"),
            "a handler return with unread branches keeps its infer anchor, got {:?}",
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
            unwidened_type_string: None,
            stated_body: None,
            printed_names: Vec::new(),
            raw_text_read: false,
            response_modes: None,
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
            unwidened_type_string: None,
            stated_body: None,
            printed_names: Vec::new(),
            raw_text_read: false,
            response_modes: None,
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
                ..
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
            unresolved_in_tree: Vec::new(),
        }
    }

    fn symbol_anchor(alias: &str) -> CaptureAnchor {
        CaptureAnchor::Symbol {
            alias: alias.to_string(),
            symbol_name: "Notification".to_string(),
            source_file: "src/routes.ts".to_string(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: None,
            consumer_response: false,
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
                printed_names: Vec::new(),
                raw_text_read: false,
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
                ..
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

    fn death(phase: Option<(&str, &str)>, end: Option<ProcessEnd>) -> CaptureDeath {
        CaptureDeath {
            error: "Sidecar process died unexpectedly".to_string(),
            progress: phase.map(|(phase, message)| OperationProgress {
                phase: phase.to_string(),
                message: message.to_string(),
            }),
            end,
        }
    }

    const ABORTED: Option<ProcessEnd> = Some(ProcessEnd::Signal(ProcessEnd::SIGABRT));
    const TWICE: &str = "the type sidecar process ended during the capture (Sidecar process \
                         died unexpectedly) and again when the capture was retried in a fresh \
                         process (Sidecar process died unexpectedly)";

    /// Two processes that aborted before any anchor was resolved are a
    /// program that does not fit the heap, and the failure says so with the
    /// cap (carrick#1916).
    #[test]
    fn two_aborts_while_building_the_program_say_the_program_did_not_fit() {
        let building = death(Some(("program", "building the program")), ABORTED);
        assert_eq!(
            two_deaths(&building, &building, Some(2048)),
            format!(
                "{TWICE}; both were aborted (SIGABRT), as Node aborts when its heap is full, \
                 while building the service's program, before any anchor was resolved: the \
                 service's program did not fit the 2048 MB heap it was given"
            )
        );
    }

    /// The stage is the last one the capture reported. A capture that got
    /// through every anchor and aborted on its last stage ran out of heap on
    /// what that stage holds, which is not the anchors.
    #[test]
    fn two_aborts_name_the_stage_they_reached_and_what_it_held() {
        let checking = death(Some(("self-check", "checking the stub")), ABORTED);
        assert_eq!(
            two_deaths(&checking, &checking, Some(8192)),
            format!(
                "{TWICE}; both were aborted (SIGABRT), as Node aborts when its heap is full, \
                 while type-checking the emitted declarations, after every anchor was \
                 resolved: the emitted declarations and the packages they import did not fit \
                 the 8192 MB heap it was given"
            )
        );
        let resolving = death(Some(("anchors", "412 of 817")), ABORTED);
        assert_eq!(
            two_deaths(&resolving, &resolving, None),
            format!(
                "{TWICE}; both were aborted (SIGABRT), as Node aborts when its heap is full, \
                 while resolving anchors (last report: 412 of 817): the service's program \
                 and the types of its anchors did not fit V8's default heap"
            )
        );
    }

    /// Nothing is said to have run out of heap unless the processes aborted:
    /// one the OS killed, or one that exited, is reported as that.
    #[test]
    fn a_death_that_was_not_an_abort_claims_no_heap() {
        let killed = death(
            Some(("emit", "emitting declarations")),
            Some(ProcessEnd::Signal(9)),
        );
        assert_eq!(
            two_deaths(&killed, &killed, Some(8192)),
            format!(
                "{TWICE}; both were killed (SIGKILL) while emitting declarations, after every \
                 anchor was resolved, under a heap cap of 8192 MB"
            )
        );
        // A sidecar that writes no frames, and a platform that reports no
        // signal: the two deaths, and nothing guessed.
        let silent = death(None, Some(ProcessEnd::Code(134)));
        assert_eq!(
            two_deaths(&silent, &silent, Some(8192)),
            format!(
                "{TWICE}; both ended with exit code 134 before reporting any progress, under \
                 a heap cap of 8192 MB"
            )
        );
        let unknown = death(None, None);
        assert_eq!(
            two_deaths(&unknown, &unknown, None),
            format!(
                "{TWICE}; both ended before reporting any progress, under V8's default heap cap"
            )
        );
    }

    /// Two deaths at different stages, or of different kinds, are told apart
    /// and nothing is concluded from them.
    #[test]
    fn two_different_deaths_are_each_described() {
        let building = death(Some(("program", "building the program")), ABORTED);
        let resolving = death(Some(("anchors", "3 of 9")), Some(ProcessEnd::Signal(9)));
        assert_eq!(
            two_deaths(&building, &resolving, Some(4096)),
            format!(
                "{TWICE}; the first was aborted (SIGABRT), as Node aborts when its heap is \
                 full, while building the service's program, before any anchor was resolved, \
                 the retry was killed (SIGKILL) while resolving anchors (last report: 3 of 9), \
                 under a heap cap of 4096 MB"
            )
        );
    }

    /// End to end through a stand-in sidecar: a capture whose process reports
    /// its stages and is killed, twice, fails with the stage it had reached
    /// and how it ended, read from the process and not guessed.
    #[test]
    fn a_capture_that_dies_twice_reports_the_stage_and_the_end_of_each_death() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let script = root.join("stand-in-sidecar.cjs");
        std::fs::write(
            &script,
            r#"
const fs = require('fs');
const write = (frame) => fs.writeSync(1, JSON.stringify(frame) + '\n');
require('readline').createInterface({ input: process.stdin, terminal: false }).on('line', (line) => {
  const request = JSON.parse(line);
  const request_id = request.request_id;
  if (request.action === 'shutdown') process.exit(0);
  if (request.action === 'init') return write({ request_id, status: 'ready' });
  write({ request_id, status: 'progress', phase: 'program', message: 'building the program the anchors are read in' });
  write({ request_id, status: 'progress', phase: 'anchors', message: '0 of 1' });
  write({ request_id, status: 'progress', phase: 'emit', message: 'emitting declarations' });
  process.kill(process.pid, 'SIGKILL');
});
"#,
        )
        .unwrap();
        let sidecar = TypeSidecar::spawn(&script).unwrap();
        sidecar.start_init(&root, None);
        sidecar
            .wait_ready(std::time::Duration::from_secs(20))
            .expect("the stand-in answers init");

        let failure = run_capture(
            &sidecar,
            root.to_str().unwrap(),
            "fixture",
            &[CaptureAnchor::Literal {
                alias: "Only".to_string(),
                type_text: "string".to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: None,
                printed_names: Vec::new(),
                raw_text_read: false,
            }],
            &HashMap::new(),
            None,
        )
        .expect_err("a capture whose process dies twice produces no stub");

        let under = match crate::services::type_sidecar::heap_cap_mb() {
            Some(mb) => format!("under a heap cap of {mb} MB"),
            None => "under V8's default heap cap".to_string(),
        };
        // Unix says which signal; elsewhere the end is an exit code.
        #[cfg(unix)]
        assert_eq!(
            failure,
            CaptureFailure::SidecarDied(format!(
                "{TWICE}; both were killed (SIGKILL) while emitting declarations, after every \
                 anchor was resolved, {under}"
            ))
        );
        let CaptureFailure::SidecarDied(detail) = failure else {
            panic!("expected the two deaths, got {failure:?}");
        };
        assert!(
            detail.contains("while emitting declarations") && detail.ends_with(&under),
            "{detail}"
        );
        assert!(
            sidecar.is_ready(),
            "a live sidecar is left for the rest of the scan"
        );
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
    /// carrick#1967: the source reads a body untyped and casts it to an array
    /// later, `const data: unknown = await response.json()` then
    /// `data as Order[]`. The model names `Order`; the inference follows the
    /// call to the body read and answers `unknown`, naming the transport's
    /// response object as its symbol. That symbol says nothing of the body, so
    /// nothing witnessed `Order` or how many of them there are, and the symbol
    /// anchor is withheld exactly as it is for an answer that names no symbol.
    #[test]
    fn derive_anchors_drops_symbol_anchor_when_the_body_was_read_untyped() {
        for top in ["unknown", "any"] {
            let explicit = vec![order_explicit("Endpoint_orders_Response")];
            let infer = vec![response_body_infer("Endpoint_orders_Response")];
            let answers = vec![inferred(
                "Endpoint_orders_Response",
                top,
                Some("Response"),
                None,
            )];
            let anchors = derive_capture_anchors(&explicit, &infer, &[], &answers, &[], "/repo");
            assert_eq!(anchors.len(), 1, "{top}: got {anchors:?}");
            assert!(
                matches!(&anchors[0], CaptureAnchor::Infer { alias, .. } if alias == "Endpoint_orders_Response"),
                "{top}: an untyped body read must leave the alias to its infer anchor, got {:?}",
                anchors[0]
            );
        }
    }

    /// The other side of that rule: a sighting keeps the anchor.
    #[test]
    fn derive_anchors_keeps_symbol_anchor_where_something_witnessed_it() {
        let alias = "Endpoint_orders_Response";
        let kept = |explicit: Vec<SymbolRequest>,
                    answers: Vec<crate::services::type_sidecar::InferredType>| {
            let anchors = derive_capture_anchors(
                &explicit,
                &[response_body_infer(alias)],
                &[],
                &answers,
                &[],
                "/repo",
            );
            matches!(&anchors[0], CaptureAnchor::Symbol { symbol_name, .. } if symbol_name == "Order")
        };
        // The answer names the anchor's own symbol: `type Order = any`.
        assert!(kept(
            vec![order_explicit(alias)],
            vec![inferred(alias, "any", Some("Order"), None)]
        ));
        // The caller knows the depth (a schema's list marker).
        assert!(kept(
            vec![SymbolRequest {
                array_depth: Some(1),
                ..order_explicit(alias)
            }],
            vec![inferred(alias, "unknown", Some("Response"), None)]
        ));
        // A second answer for the alias saw a shape.
        assert!(kept(
            vec![order_explicit(alias)],
            vec![
                inferred(alias, "unknown", Some("Response"), None),
                inferred(alias, "{ id: string; }", None, None),
            ]
        ));
        // No inference ran for the alias: nothing looked, so nothing is blind.
        assert!(kept(vec![order_explicit(alias)], Vec::new()));
    }

    /// carrick#1967: a backfill re-anchors a demoted alias with the scanner's
    /// own text for it, and the explicit bundle's text IS the model symbol's
    /// expansion. Where the symbol anchor is withheld because nothing
    /// witnessed the symbol, offering that text publishes the same guess one
    /// step later: one `Order` where the source reads a list of them.
    #[test]
    fn derive_backfill_texts_offers_no_symbol_text_nothing_witnessed() {
        let entry = |alias: &str| ManifestEntry {
            alias: alias.to_string(),
            original_name: "Order".to_string(),
            source_file: "src/types.ts".to_string(),
            type_string: "{ id: string; total: number; }".to_string(),
            is_explicit: true,
        };
        let manifest = vec![
            entry("Untyped"),
            entry("Blind"),
            entry("Named"),
            entry("Listed"),
            entry("Unasked"),
        ];
        let answers = vec![
            // The body read untyped: the transport's response object beside a bare top type.
            inferred("Untyped", "unknown", Some("Response"), None),
            // The answer that was blind before this rule: no symbol at all.
            inferred("Blind", "any", None, None),
            inferred("Named", "any", Some("Order"), None),
            inferred("Listed", "unknown", Some("Response"), None),
        ];
        let requests = vec![
            order_explicit("Untyped"),
            order_explicit("Blind"),
            order_explicit("Named"),
            SymbolRequest {
                array_depth: Some(1),
                ..order_explicit("Listed")
            },
            order_explicit("Unasked"),
        ];
        let texts = derive_backfill_texts(&manifest, &answers, &requests);
        let mut offered: Vec<&str> = texts.keys().map(String::as_str).collect();
        offered.sort_unstable();
        assert_eq!(offered, ["Listed", "Named", "Unasked"]);

        // And nothing is re-anchored for an alias with no text: its infer
        // anchor demoted, and it stays the honest unknown it was.
        let anchors = derive_capture_anchors(
            &[order_explicit("Untyped")],
            &[response_body_infer("Untyped")],
            &[],
            &[inferred("Untyped", "unknown", Some("Response"), None)],
            &[],
            "/repo",
        );
        let demoted = vec![record(
            "Untyped",
            "infer",
            "decayed_internal",
            Some("locator resolved a top type"),
        )];
        assert!(backfill_anchors(&anchors, &demoted, &texts).is_none());
    }

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
            unwidened_type_string: None,
            stated_body: None,
            printed_names: Vec::new(),
            raw_text_read: false,
            response_modes: None,
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

        let texts = derive_backfill_texts(&explicit, &inferred, &[]);
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

    /// carrick#1967: the sidecar reports an array level whether or not the
    /// element has a symbol, and the level can describe a value other than
    /// the one the text prints (a call's result beside a decayed read). Where
    /// the depth join copied nothing onto the request, a depth with no element
    /// named is no sighting of the model symbol: a read that decayed to a bare
    /// top type still withholds the symbol anchor, as it did before.
    #[test]
    fn an_unnamed_depth_beside_a_decayed_read_is_no_sighting() {
        let alias = "Endpoint_orders_Request";
        let mut read = inferred(alias, "unknown", None, Some(1));
        read.infer_kind = InferKind::RequestBody;
        let anchors = derive_capture_anchors(
            &[order_explicit(alias)],
            &[response_body_infer(alias)],
            &[],
            &[read],
            &[],
            "/repo",
        );
        assert!(
            !anchors
                .iter()
                .any(|anchor| matches!(anchor, CaptureAnchor::Symbol { .. })),
            "{anchors:?}"
        );
    }

    // ---- build_check_pairs ------------------------------------------------

    /// HTTP keeps only the most specific producer for a consumer, and the
    /// consumer's own service is ranked with the rest (carrick#1945): a
    /// sibling's literal route beats the caller's own parameterized one, and
    /// the caller's own literal route beats a sibling's parameterized one, in
    /// which case the pair names one service twice.
    #[test]
    fn build_pairs_http_specificity_ranks_the_consumers_own_routes() {
        let surfaced = |name: &str, entries: Vec<TypeManifestEntry>| {
            repo(name, None, entries, Some(fake_artifact()))
        };
        let producer = |path: &str, alias: &str, line: u32| {
            entry(
                OperationKey::http("GET", path),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                alias,
                "src/routes.ts",
                line,
                ManifestTypeState::Explicit,
            )
        };
        let consumer = entry(
            OperationKey::http("GET", "/users/me"),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "C_me",
            "src/client.ts",
            12,
            ManifestTypeState::Explicit,
        );

        // The sibling states the literal route; the caller's own is a param.
        let pairs = build_check_pairs(&[
            surfaced("api", vec![producer("/users/me", "P_api_literal", 9)]),
            surfaced(
                "web",
                vec![producer("/users/:id", "P_web_param", 3), consumer.clone()],
            ),
        ]);
        assert_eq!(pairs.len(), 1, "one pair: the most specific producer wins");
        let pair = &pairs[0];
        assert_eq!(pair.producer_alias, "P_api_literal");
        assert_eq!(pair.producer_service, "api");
        assert_eq!(pair.consumer_alias, "C_me");
        assert_eq!(pair.consumer_service, "web");
        assert_eq!(pair.pseudo_method, "GET");
        assert_eq!(pair.identity, "/users/me");
        assert_eq!(pair.consumer_file, "src/client.ts");
        assert_eq!(pair.consumer_line, 12);
        assert!(pair.pre_verdict.is_none());
        assert_eq!(pair.spec.protocol, ProbeProtocol::Http);
        assert_eq!(pair.spec.type_kind, ProbeTypeKind::Response);

        // The caller's own route is the literal one: the pair is same-service.
        let pairs = build_check_pairs(&[
            surfaced("api", vec![producer("/users/:id", "P_api_param", 3)]),
            surfaced(
                "web",
                vec![producer("/users/me", "P_web_literal", 9), consumer.clone()],
            ),
        ]);
        assert_eq!(pairs.len(), 1, "one pair: the most specific producer wins");
        let pair = &pairs[0];
        assert_eq!(pair.producer_alias, "P_web_literal");
        assert_eq!(pair.producer_service, "web");
        assert_eq!(pair.consumer_service, "web");
        assert_eq!(pair.spec.producer.service_name, "web");
        assert_eq!(pair.spec.consumer.service_name, "web");
        assert!(pair.pre_verdict.is_none());

        // A one-service project: its call to its own route is paired.
        let pairs = build_check_pairs(&[surfaced(
            "web",
            vec![producer("/users/:id", "P_web_param", 3), consumer],
        )]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].producer_alias, "P_web_param");
        assert_eq!(pairs[0].consumer_alias, "C_me");
    }

    /// carrick#1945: only a same-service `incompatible` is held back. Its
    /// half reads unverifiable with the one sentence; a same-service
    /// `compatible` and `unverifiable`, and a mismatch between two services,
    /// are exactly as the check left them.
    #[test]
    fn a_same_service_mismatch_is_published_as_not_reported_and_nothing_else_moves() {
        let outcome = |producer: &str, consumer: &str, bucket: VerdictBucket| PairCheckOutcome {
            pair_key: format!("{producer}~{consumer}~{bucket:?}"),
            pseudo_method: "GET".to_string(),
            identity: "/api/items".to_string(),
            consumer_file: "components/list.tsx".to_string(),
            consumer_line: 12,
            type_kind: ManifestTypeKind::Response,
            bucket,
            gate: (bucket == VerdictBucket::Unverifiable).then(|| "consumer:unknown".to_string()),
            diagnostic: (bucket != VerdictBucket::Compatible)
                .then(|| "the check's text".to_string()),
            producer_alias: "P".to_string(),
            consumer_alias: "C".to_string(),
            producer_service: producer.to_string(),
            consumer_service: consumer.to_string(),
            resolved: bucket != VerdictBucket::Unverifiable,
            unresolved_reason: (bucket == VerdictBucket::Unverifiable)
                .then(|| "the consumer type is 'unknown'".to_string()),
            notes: vec!["a note".to_string()],
            consumer_reads: if bucket == VerdictBucket::Incompatible {
                vec![14]
            } else {
                Vec::new()
            },
        };
        let before = vec![
            outcome("app", "app", VerdictBucket::Incompatible),
            outcome("app", "app", VerdictBucket::Compatible),
            outcome("app", "app", VerdictBucket::Unverifiable),
            outcome("api", "web", VerdictBucket::Incompatible),
        ];
        let mut after = before.clone();
        hold_back_same_service_mismatches(&mut after);

        let held = &after[0];
        assert_eq!(held.bucket, VerdictBucket::Unverifiable);
        assert_eq!(held.gate.as_deref(), Some("same_service"));
        assert_eq!(
            held.unresolved_reason.as_deref(),
            Some(SAME_SERVICE_MISMATCH_NOT_REPORTED)
        );
        assert!(!held.resolved);
        // What the check found stays on the outcome (carrick#2053), for the
        // run log. Its notes and the consumer lines do not ride a held-back
        // half, as before.
        assert_eq!(held.diagnostic.as_deref(), Some("the check's text"));
        assert!(held.notes.is_empty() && held.consumer_reads.is_empty());
        assert_eq!(
            format!("{:?}", &after[1..]),
            format!("{:?}", &before[1..]),
            "nothing else moves"
        );

        // Stored, the half reads unverifiable with the sentence, and the
        // edge carries no mismatch.
        let directions = crate::analyzer::PairDirections::from_outcomes(&after[..1]);
        let edge = crate::analyzer::CrossRepoMatch {
            producer_repo: "app".to_string(),
            producer_key: "http|GET|/api/items".to_string(),
            consumer_repo: "app".to_string(),
            consumer_key: "http|GET|/api/items".to_string(),
            consumer_location: Some("components/list.tsx:12".to_string()),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        };
        let response = directions
            .for_edge(&edge)
            .response
            .expect("the half is stored");
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Unverifiable
        );
        assert_eq!(response.reason, None);
        assert_eq!(
            response.unresolved_reason.as_deref(),
            Some(SAME_SERVICE_MISMATCH_NOT_REPORTED)
        );
        assert!(
            response.notes.is_empty(),
            "a note reaches agents; the finding is for the run log"
        );
        let mut edges = vec![edge];
        crate::analyzer::apply_pair_outcomes(&after[..1], &mut edges);
        assert_eq!(edges[0].type_compatible, None);
        assert_eq!(edges[0].mismatch_reason, None);
    }

    /// A same-service outcome built from the retype helper's pair, which
    /// pairs two services: the producer is moved onto the consumer's service.
    fn same_service_outcome(
        bucket: VerdictBucket,
        diagnostic: Option<&str>,
        notes: Vec<String>,
    ) -> PairCheckOutcome {
        let pair = retype_pair(ManifestTypeKind::Response, Some("{ y: number; }"));
        let mut outcome = outcome_for(
            &pair,
            bucket,
            None,
            diagnostic.map(str::to_string),
            bucket != VerdictBucket::Unverifiable,
            None,
            notes,
        );
        outcome.producer_service = outcome.consumer_service.clone();
        outcome
    }

    /// The half the blob stores for one outcome, as the stored row reads it.
    fn stored_response_half(outcome: &PairCheckOutcome) -> crate::cloud_storage::DirectionVerdict {
        let directions =
            crate::analyzer::PairDirections::from_outcomes(std::slice::from_ref(outcome));
        let key = format!("http|{}|{}", outcome.pseudo_method, outcome.identity);
        let edge = crate::analyzer::CrossRepoMatch {
            producer_repo: outcome.producer_service.clone(),
            producer_key: key.clone(),
            consumer_repo: outcome.consumer_service.clone(),
            consumer_key: key,
            consumer_location: Some(format!(
                "{}:{}",
                outcome.consumer_file, outcome.consumer_line
            )),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        };
        let half = directions
            .for_edge(&edge)
            .response
            .expect("the half is stored");
        crate::cloud_storage::direction_verdict(&half)
    }

    /// carrick#2053: a same-service mismatch the check found is held back,
    /// and what it found stays on the outcome for the run log. What a reader
    /// of the index sees is exactly what it was: the half is unverifiable
    /// with the sentence as its reason, and carries no `reason` (a reason is
    /// read as a mismatch) and no note (a note reaches agents).
    #[test]
    fn a_held_back_mismatch_keeps_what_the_check_found_off_the_stored_half() {
        let mut outcomes = vec![same_service_outcome(
            VerdictBucket::Incompatible,
            Some("Property 'y' is missing in type '{ x: string; }'."),
            vec!["a note the check made".to_string()],
        )];
        hold_back_same_service_mismatches(&mut outcomes);

        assert_eq!(
            outcomes[0].diagnostic.as_deref(),
            Some("Property 'y' is missing in type '{ x: string; }'.")
        );
        assert_eq!(
            stored_response_half(&outcomes[0]),
            crate::cloud_storage::DirectionVerdict {
                verdict: crate::operation::TypeVerdict::Unverifiable,
                reason: None,
                resolved: false,
                unresolved_reason: Some(SAME_SERVICE_MISMATCH_NOT_REPORTED.to_string()),
                notes: Vec::new(),
                producer_wider: false,
            }
        );
    }

    /// carrick#2053: a mismatch the retype found names the consumer's own
    /// reads, and those lines stay in the outcome's text for the run log. The
    /// consumer lines are cleared as such, since a non-empty list is read as
    /// the places of a finding, which a held-back half is not.
    #[test]
    fn a_held_back_retype_mismatch_keeps_the_consumer_lines_in_its_text() {
        let mut outcome = same_service_outcome(VerdictBucket::Unverifiable, None, Vec::new());
        outcome.unresolved_reason = Some("the consumer type is 'unknown'".to_string());
        let item_id = outcome.pair_key.clone();
        apply_retype(
            &mut outcome,
            &RetypeOutcome {
                item_id,
                outcome: RetypeVerdict::Mismatch,
                diagnostics: vec![
                    crate::services::type_sidecar::RetypeDiagnostic {
                        line: 9,
                        code: 2339,
                        message: "Property 'x' does not exist on type '{ y: number; }'."
                            .to_string(),
                    },
                    crate::services::type_sidecar::RetypeDiagnostic {
                        line: 14,
                        code: 2339,
                        message: "Property 'z' does not exist on type '{ y: number; }'."
                            .to_string(),
                    },
                ],
                reason: None,
            },
        );
        assert_eq!(outcome.consumer_reads, vec![9, 14]);
        let mut outcomes = vec![outcome];
        hold_back_same_service_mismatches(&mut outcomes);

        let held = &outcomes[0];
        assert_eq!(held.bucket, VerdictBucket::Unverifiable);
        assert!(held.consumer_reads.is_empty());
        assert_eq!(
            held.diagnostic.as_deref(),
            Some(
                "the consumer uses what the producer's response does not provide: \
                 src/client.ts:9: Property 'x' does not exist on type '{ y: number; }'.; \
                 src/client.ts:14: Property 'z' does not exist on type '{ y: number; }'."
            )
        );
        // The retype's note is cleared with the rest, as before.
        assert!(held.notes.is_empty());
        let stored = stored_response_half(held);
        assert_eq!(stored.verdict, crate::operation::TypeVerdict::Unverifiable);
        assert_eq!(stored.reason, None);
        assert!(stored.notes.is_empty());
    }

    /// carrick#2053: a mismatch that came back with no text has nothing to
    /// keep, so the outcome carries none rather than an empty string.
    #[test]
    fn a_held_back_mismatch_with_no_text_keeps_no_text() {
        for diagnostic in [None, Some("")] {
            let mut outcomes = vec![same_service_outcome(
                VerdictBucket::Incompatible,
                diagnostic,
                Vec::new(),
            )];
            hold_back_same_service_mismatches(&mut outcomes);
            assert_eq!(outcomes[0].bucket, VerdictBucket::Unverifiable);
            assert_eq!(outcomes[0].diagnostic, None, "{diagnostic:?}");
        }
    }

    /// carrick#2053: the CI log's line for a held-back pair says why it is
    /// held back, then what the check found. Another unverified pair's line is
    /// its reason alone, whatever its diagnostic holds.
    #[test]
    fn a_held_back_pair_is_logged_with_the_sentence_then_what_the_check_found() {
        let mut held = vec![same_service_outcome(
            VerdictBucket::Incompatible,
            Some("Property 'y' is missing"),
            Vec::new(),
        )];
        hold_back_same_service_mismatches(&mut held);
        assert_eq!(
            unresolved_pair_line(&held[0]).as_deref(),
            Some(
                "Types not verified: POST /p response (src/client.ts:8 in web against web): \
                 a mismatch between a call and a route of its own service, which Carrick does \
                 not report yet; what the check found: Property 'y' is missing"
            )
        );

        // A compiler's chain is one line in the log and verbatim on the outcome.
        let chain = "Type 'A' is not assignable to type 'B'.\n  Types of property 'id' are \
                     incompatible.\n    Type 'string' is not assignable to type 'number'.";
        let mut chained = vec![same_service_outcome(
            VerdictBucket::Incompatible,
            Some(chain),
            Vec::new(),
        )];
        hold_back_same_service_mismatches(&mut chained);
        let line = unresolved_pair_line(&chained[0]).expect("a line");
        assert!(!line.contains('\n'), "{line}");
        assert!(
            line.ends_with(
                "; what the check found: Type 'A' is not assignable to type 'B'. Types of \
                 property 'id' are incompatible. Type 'string' is not assignable to type 'number'."
            ),
            "{line}"
        );
        assert_eq!(chained[0].diagnostic.as_deref(), Some(chain));

        let mut nothing_found = vec![same_service_outcome(
            VerdictBucket::Incompatible,
            None,
            Vec::new(),
        )];
        hold_back_same_service_mismatches(&mut nothing_found);
        assert_eq!(
            unresolved_pair_line(&nothing_found[0]).as_deref(),
            Some(
                "Types not verified: POST /p response (src/client.ts:8 in web against web): \
                 a mismatch between a call and a route of its own service, which Carrick does \
                 not report yet"
            )
        );

        let mut other = same_service_outcome(
            VerdictBucket::GateCaughtBakedAny,
            Some("the check's text"),
            vec!["a note".to_string()],
        );
        other.gate = Some("capture:consumer:any".to_string());
        other.resolved = false;
        other.unresolved_reason = Some("the consumer type carries 'any' at '<1>'".to_string());
        assert_eq!(
            unresolved_pair_line(&other).as_deref(),
            Some(
                "Types not verified: POST /p response (src/client.ts:8 in web against web): \
                 the consumer type carries 'any' at '<1>'"
            )
        );
    }

    /// carrick#2004: a route that states a literal where the call has a
    /// placeholder is never paired, because which route the call reaches is
    /// unknown. A call path that is literal there keeps its pair, and a
    /// placeholder route at that position is still the call's route.
    #[test]
    fn a_call_placeholder_over_a_route_literal_makes_no_pair() {
        let producer = |method: &str, path: &str, alias: &str| {
            entry(
                OperationKey::http(method, path),
                ManifestRole::Producer,
                ManifestTypeKind::Request,
                alias,
                "pages/api/routes.ts",
                1,
                ManifestTypeState::Explicit,
            )
        };
        let consumer = |method: &str, path: &str, alias: &str| {
            entry(
                OperationKey::http(method, path),
                ManifestRole::Consumer,
                ManifestTypeKind::Request,
                alias,
                "components/form.tsx",
                9,
                ManifestTypeState::Explicit,
            )
        };
        let paired = |entries: Vec<TypeManifestEntry>| -> Vec<(String, String)> {
            build_check_pairs(&[repo("app", None, entries, Some(fake_artifact()))])
                .into_iter()
                .map(|pair| (pair.consumer_alias, pair.producer_alias))
                .collect()
        };
        let siblings = || {
            vec![
                producer("POST", "/api/orgs/:orgId/folders", "P_folders"),
                producer("POST", "/api/orgs/:orgId/documents", "P_documents"),
                producer("POST", "/api/orgs/:orgId/agreements", "P_agreements"),
            ]
        };

        // A placeholder last segment against three literal siblings: no pair.
        let mut entries = siblings();
        entries.push(consumer("POST", "/api/orgs/:id/:target", "C_target"));
        assert_eq!(paired(entries), vec![]);

        // The same call with its last segment literal keeps its one pair.
        let mut entries = siblings();
        entries.push(consumer("POST", "/api/orgs/:id/folders", "C_folders"));
        assert_eq!(
            paired(entries),
            vec![("C_folders".to_string(), "P_folders".to_string())]
        );

        // `:id` against a literal `move` alone: no pair.
        let move_only = vec![
            producer("PATCH", "/api/things/move", "P_move"),
            consumer("PATCH", "/api/things/:id", "C_thing"),
        ];
        assert_eq!(paired(move_only), vec![]);

        // With an `:id` route beside it, the `:id` route only.
        let both = vec![
            producer("PATCH", "/api/things/move", "P_move"),
            producer("PATCH", "/api/things/:thingId", "P_thing"),
            consumer("PATCH", "/api/things/:id", "C_thing"),
        ];
        assert_eq!(
            paired(both),
            vec![("C_thing".to_string(), "P_thing".to_string())]
        );
    }

    /// One HTTP row at `site` (`file:line`), as the index states an endpoint
    /// or a call, answering or sending `op = value` when given.
    fn dispatch_row(
        method: &str,
        path: &str,
        site: &str,
        value: Option<&str>,
    ) -> crate::analyzer::ApiEndpointDetails {
        crate::analyzer::ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::http(method, path),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(site),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: value.map(|value| crate::dispatch::Dispatch {
                location: crate::dispatch::DispatchLocation::Body,
                field: "op".to_string(),
                value: value.to_string(),
            }),
            schema_binding: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
        }
    }

    /// The pairs the check builds for one service whose `POST` route answers
    /// a different body for each value of `op` (carrick#2059). The case
    /// `confirm` has a site of its own; `cancel` and `refund` share the
    /// route's site, so one producer entry stands for both. Its calls send
    /// `confirm`, `refund`, a value no case answers (`archive`), and nothing.
    /// A plain `GET` beside it is called with a body that happens to carry
    /// `op` too.
    fn pairs_for_a_dispatching_route() -> Vec<BuiltPair> {
        const ROUTE: &str = "/api/orders/:orderId";
        const HANDLER: &str = "app/api/orders/[orderId]/route.ts";
        const CLIENT: &str = "components/OrderActions.tsx";
        let manifest_entry = |method: &str, role, alias: &str, file: &str, line| {
            entry(
                OperationKey::http(method, ROUTE),
                role,
                ManifestTypeKind::Response,
                alias,
                file,
                line,
                ManifestTypeState::Explicit,
            )
        };
        let site = |file: &str, line: u32| format!("{file}:{line}");
        let mut app = repo(
            "app",
            None,
            vec![
                manifest_entry("POST", ManifestRole::Producer, "P_confirm", HANDLER, 17),
                manifest_entry("POST", ManifestRole::Producer, "P_route", HANDLER, 9),
                manifest_entry("GET", ManifestRole::Producer, "P_get", HANDLER, 4),
                manifest_entry("POST", ManifestRole::Consumer, "C_confirm", CLIENT, 5),
                manifest_entry("POST", ManifestRole::Consumer, "C_refund", CLIENT, 6),
                manifest_entry("POST", ManifestRole::Consumer, "C_archive", CLIENT, 7),
                manifest_entry("POST", ManifestRole::Consumer, "C_none", CLIENT, 8),
                manifest_entry("GET", ManifestRole::Consumer, "C_get", CLIENT, 9),
            ],
            Some(fake_artifact()),
        );
        app.endpoints = vec![
            dispatch_row("POST", ROUTE, &site(HANDLER, 17), Some("confirm")),
            dispatch_row("POST", ROUTE, &site(HANDLER, 9), Some("cancel")),
            dispatch_row("POST", ROUTE, &site(HANDLER, 9), Some("refund")),
            dispatch_row("GET", ROUTE, &site(HANDLER, 4), None),
        ];
        app.calls = vec![
            dispatch_row("POST", ROUTE, &site(CLIENT, 5), Some("confirm")),
            dispatch_row("POST", ROUTE, &site(CLIENT, 6), Some("refund")),
            dispatch_row("POST", ROUTE, &site(CLIENT, 7), Some("archive")),
            dispatch_row("POST", ROUTE, &site(CLIENT, 8), None),
            dispatch_row("GET", ROUTE, &site(CLIENT, 9), Some("confirm")),
        ];
        build_check_pairs(&[app])
    }

    /// The check pairs a call to a dispatching route with what the analyzer's
    /// matcher pairs it with (`carrick_match::dispatch_outcome`), so every
    /// half it judges is a half the index stores (carrick#2059): the case the
    /// call sends, found through a producer entry that stands for two cases;
    /// nothing for a value no case answers; and a route that does not
    /// dispatch, whatever the call's body carries.
    #[test]
    fn a_call_to_a_dispatching_route_is_paired_as_the_matcher_pairs_it() {
        let mut paired: Vec<(String, String)> = pairs_for_a_dispatching_route()
            .into_iter()
            .filter(|pair| pair.pre_verdict.is_none())
            .map(|pair| (pair.consumer_alias, pair.producer_alias))
            .collect();
        paired.sort();
        assert_eq!(
            paired,
            vec![
                ("C_confirm".to_string(), "P_confirm".to_string()),
                ("C_get".to_string(), "P_get".to_string()),
                ("C_refund".to_string(), "P_route".to_string()),
            ]
        );
    }

    /// A call that states no case keeps its edge to the route with the case
    /// unknown, so it has one half per kind, not one per case. Nothing is
    /// probed: the half is stored unverifiable and says why, and it is never
    /// sent to the retype check, which would judge it against one case's type.
    #[test]
    fn a_call_that_states_no_case_has_one_unverifiable_half_and_no_probe() {
        let unknown: Vec<BuiltPair> = pairs_for_a_dispatching_route()
            .into_iter()
            .filter(|pair| pair.consumer_alias == "C_none")
            .collect();
        assert_eq!(unknown.len(), 1, "one half for the route, not one per case");
        assert_eq!(
            unknown[0].pre_verdict,
            Some((
                VerdictBucket::Unverifiable,
                "Not compared: this route returns a different response depending on `op` in \
                 the request, and Carrick cannot tell which one this call receives."
                    .to_string()
            ))
        );
        assert_eq!(unknown[0].pre_verdict_side, None);
    }

    /// Only HTTP pairs a service with itself. The exact-key matcher drops a
    /// same-service edge for GraphQL, socket and pub/sub (#397/#410), so a
    /// pair of those would be judged against no edge (carrick#1945).
    #[test]
    fn build_pairs_exact_key_protocols_stay_between_two_services() {
        let keys = [
            OperationKey::graphql(crate::operation::GraphqlOperationKind::Query, "orders"),
            OperationKey::socket(
                "order:placed",
                crate::operation::SocketDirection::ClientToServer,
            ),
            OperationKey::pubsub("order.placed"),
        ];
        for key in keys {
            let one_service = repo(
                "orders",
                None,
                vec![
                    entry(
                        key.clone(),
                        ManifestRole::Producer,
                        ManifestTypeKind::Response,
                        "P",
                        "src/server.ts",
                        4,
                        ManifestTypeState::Explicit,
                    ),
                    entry(
                        key.clone(),
                        ManifestRole::Consumer,
                        ManifestTypeKind::Response,
                        "C",
                        "src/client.ts",
                        21,
                        ManifestTypeState::Explicit,
                    ),
                ],
                Some(fake_artifact()),
            );
            assert!(
                build_check_pairs(&[one_service]).is_empty(),
                "{key:?} pairs a service with itself"
            );
        }
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
        let Some((sidecar, key, all_repo_data)) = corpus_pair() else {
            return;
        };

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

    /// The corpus-2 fixture pair captured through the Rust client, as
    /// `run_check` reads it: a sidecar init'd on the producer, the pair's
    /// operation, and both services' repo data. `None` when the sidecar is
    /// not built.
    fn corpus_pair() -> Option<(TypeSidecar, OperationKey, Vec<CloudRepoData>)> {
        corpus_pair_with(&[])
    }

    /// One more response pair beside the corpus-2 pair: its operation, and the
    /// literal type text each side is captured from.
    struct LiteralPair {
        key: OperationKey,
        producer_type: &'static str,
        consumer_type: &'static str,
    }

    /// `corpus_pair`, with each of `extra` captured into the same two stubs as
    /// one more matched pair.
    fn corpus_pair_with(
        extra: &[LiteralPair],
    ) -> Option<(TypeSidecar, OperationKey, Vec<CloudRepoData>)> {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return None;
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
        let mut orders_anchors = vec![CaptureAnchor::Symbol {
            alias: producer_alias.clone(),
            symbol_name: "OrderPlaced".to_string(),
            source_file: "src/types/order.ts".to_string(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: None,
            consumer_response: false,
        }];
        let mut billing_anchors = vec![CaptureAnchor::Symbol {
            alias: consumer_alias.clone(),
            symbol_name: "OrderPlaced".to_string(),
            source_file: "src/types/billing.ts".to_string(),
            anchor_origin: AnchorOrigin::LlmSymbol,
            array_depth: None,
            consumer_response: false,
        }];
        let mut orders_manifest = vec![entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            &producer_alias,
            "src/routes.ts",
            3,
            ManifestTypeState::Explicit,
        )];
        let mut billing_manifest = vec![entry(
            key.clone(),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            &consumer_alias,
            "src/billing-call.ts",
            5,
            ManifestTypeState::Explicit,
        )];
        for (index, pair) in extra.iter().enumerate() {
            let line = 100 + index as u32;
            let producer = build_manifest_type_alias(
                &pair.key,
                ManifestRole::Producer,
                ManifestTypeKind::Response,
            );
            let site = crate::type_manifest::build_site_id(
                "src/billing-call.ts",
                line,
                &pair.key,
                billing_repo.to_str().unwrap(),
            );
            let consumer = build_manifest_type_alias_with_site_id(
                &pair.key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                Some(&site),
            );
            orders_anchors.push(CaptureAnchor::Literal {
                alias: producer.clone(),
                type_text: pair.producer_type.to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: None,
                printed_names: Vec::new(),
                raw_text_read: false,
            });
            billing_anchors.push(CaptureAnchor::Literal {
                alias: consumer.clone(),
                type_text: pair.consumer_type.to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: None,
                printed_names: Vec::new(),
                raw_text_read: false,
            });
            orders_manifest.push(entry(
                pair.key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                &producer,
                "src/routes.ts",
                line,
                ManifestTypeState::Explicit,
            ));
            billing_manifest.push(entry(
                pair.key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                &consumer,
                "src/billing-call.ts",
                line,
                ManifestTypeState::Explicit,
            ));
        }
        let (orders_stub, orders_artifact) = run_capture(
            &sidecar,
            orders_repo.to_str().unwrap(),
            "orders-engine",
            &orders_anchors,
            &HashMap::new(),
            None,
        )
        .expect("orders-engine capture");
        let (billing_stub, billing_artifact) = run_capture(
            &sidecar,
            billing_repo.to_str().unwrap(),
            "billing-svc",
            &billing_anchors,
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
                orders_manifest,
                Some(orders_artifact),
            ),
            repo(
                "billing-svc",
                None,
                billing_manifest,
                Some(billing_artifact),
            ),
        ];
        Some((sidecar, key, all_repo_data))
    }

    /// carrick#1945, end to end through the real sidecar: a service whose
    /// calls reach its own route is checked with its one stub on both sides
    /// of each pair, by the same judge as two services. A consumer read as
    /// `any` is unverifiable and says why, never compatible; a consumer that
    /// agrees is compatible; one that disagrees is found by the check and
    /// published as unverifiable, not reported yet.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_service_paired_with_itself_is_judged_by_the_same_check() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let root = manifest_dir.join("tests/fixtures/xrepo-corpus-2/orders-engine");
        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("GET", "/api/items");
        let producer =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let literal = |alias: &str, type_text: &str| CaptureAnchor::Literal {
            alias: alias.to_string(),
            type_text: type_text.to_string(),
            anchor_origin: AnchorOrigin::DeterministicInfer,
            source_file: None,
            printed_names: Vec::new(),
            raw_text_read: false,
        };
        let mut anchors = vec![literal(&producer, "{ items: { id: string; }[]; }")];
        let mut manifest = vec![entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            &producer,
            "app/api/items/route.ts",
            3,
            ManifestTypeState::Explicit,
        )];
        let consumers = [
            (10, "any"),
            (20, "{ items: { id: string; }[]; }"),
            (30, "{ items: { id: number; }[]; }"),
        ];
        for (line, type_text) in consumers {
            let site = crate::type_manifest::build_site_id(
                "components/list.tsx",
                line,
                &key,
                root.to_str().unwrap(),
            );
            let alias = build_manifest_type_alias_with_site_id(
                &key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                Some(&site),
            );
            anchors.push(literal(&alias, type_text));
            manifest.push(entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                &alias,
                "components/list.tsx",
                line,
                ManifestTypeState::Explicit,
            ));
        }
        let (stub, artifact) = run_capture(
            &sidecar,
            root.to_str().unwrap(),
            "app",
            &anchors,
            &HashMap::new(),
            None,
        )
        .expect("capture");
        let _ = std::fs::remove_dir_all(&stub);
        let all_repo_data = vec![repo("app", None, manifest, Some(artifact))];

        let mut outcomes = run_check(&sidecar, &all_repo_data, &LocalConsumers::new());
        outcomes.sort_by_key(|outcome| outcome.consumer_line);
        // A gate that caught the `any` is stored as unverifiable too.
        let stored = |bucket: VerdictBucket| match bucket {
            VerdictBucket::GateCaughtBakedAny => VerdictBucket::Unverifiable,
            other => other,
        };
        let read: Vec<(u32, &str, &str, VerdictBucket)> = outcomes
            .iter()
            .map(|o| {
                (
                    o.consumer_line,
                    o.producer_service.as_str(),
                    o.consumer_service.as_str(),
                    stored(o.bucket),
                )
            })
            .collect();
        assert_eq!(
            read,
            vec![
                (10, "app", "app", VerdictBucket::Unverifiable),
                (20, "app", "app", VerdictBucket::Compatible),
                (30, "app", "app", VerdictBucket::Unverifiable),
            ],
            "{outcomes:#?}"
        );
        // The mismatch the check found is published as not reported, and
        // says so; what it found stays on the outcome for the run log
        // (carrick#2053) and on neither the half's reason nor its notes.
        let held = &outcomes[2];
        assert_eq!(held.gate.as_deref(), Some("same_service"));
        assert_eq!(
            held.unresolved_reason.as_deref(),
            Some(SAME_SERVICE_MISMATCH_NOT_REPORTED)
        );
        assert!(!held.resolved && held.notes.is_empty() && held.consumer_reads.is_empty());
        let found = held.diagnostic.as_deref().unwrap_or_default();
        assert!(
            found.contains("'id'") && found.contains("string") && found.contains("number"),
            "the outcome names the field the check found: {held:#?}"
        );
        let line = unresolved_pair_line(held).expect("a held-back pair is logged");
        assert!(
            line.contains(SAME_SERVICE_MISMATCH_NOT_REPORTED)
                && line.contains("; what the check found: ")
                && line.contains("'id'")
                && !line.contains('\n'),
            "{line}"
        );
        let stored = stored_response_half(held);
        assert_eq!(stored.reason, None, "a finding is never the stored reason");
        assert!(stored.notes.is_empty(), "{stored:#?}");
        let any = &outcomes[0];
        assert!(!any.resolved);
        assert!(
            any.unresolved_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty()),
            "an unverifiable pair says why: {any:#?}"
        );
        assert!(outcomes[1].resolved, "{outcomes:#?}");
    }

    /// carrick#1821: when the check workspace's install fails, every pair the
    /// check would have compared says why, in the installer's own words, not
    /// "sidecar returned an error with no detail". The producer's stub names a
    /// dependency at a directory that does not exist, which the vendored pnpm
    /// refuses before it reaches any registry.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_failed_install_reaches_the_pair_in_the_installers_words() {
        let Some((sidecar, _key, mut all_repo_data)) = corpus_pair() else {
            return;
        };
        break_the_install(&mut all_repo_data);

        let outcomes = run_check(&sidecar, &all_repo_data, &LocalConsumers::new());
        assert_eq!(outcomes.len(), 1, "exactly one matched pair");
        assert_reads_the_installers_words(&outcomes[0]);
    }

    /// carrick#1833: a failed install leaves standing every verdict the
    /// capture had already decided. The sidecar still answers one verdict per
    /// pair when its install fails; a pair whose consumer type carries a deep
    /// `any` keeps `gate_caught_baked_any`, while the pair the check would
    /// have probed says the install stopped it.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_failed_install_keeps_the_verdicts_the_capture_decided() {
        let Some((sidecar, _key, mut all_repo_data)) = corpus_pair_with(&[LiteralPair {
            key: OperationKey::http("GET", "/orders/summary"),
            producer_type: "{ id: string; total: number }",
            consumer_type: "{ id: string; total: any }",
        }]) else {
            return;
        };
        break_the_install(&mut all_repo_data);

        let outcomes = run_check(&sidecar, &all_repo_data, &LocalConsumers::new());
        assert_eq!(outcomes.len(), 2, "two matched pairs: {outcomes:?}");
        let outcome_of = |path: &str| {
            outcomes
                .iter()
                .find(|o| o.identity == path)
                .unwrap_or_else(|| panic!("no outcome for {path}: {outcomes:?}"))
        };

        let decided = outcome_of("/orders/summary");
        assert_eq!(
            decided.bucket,
            VerdictBucket::GateCaughtBakedAny,
            "the capture's verdict stands: {decided:?}"
        );
        assert_eq!(decided.gate.as_deref(), Some("capture:consumer:any"));
        let reason = decided.unresolved_reason.as_deref().unwrap_or("");
        assert!(
            reason.contains("'any'") && !reason.contains("install"),
            "the reason is the capture's, not the install's: {reason}"
        );

        assert_reads_the_installers_words(outcome_of("/orders/latest"));
    }

    /// Gives the producer's stub a dependency at a directory that does not
    /// exist, which the vendored pnpm refuses before it reaches any registry.
    fn break_the_install(all_repo_data: &mut [CloudRepoData]) {
        let manifest = all_repo_data[0]
            .capture_stub
            .as_mut()
            .expect("producer artifact")
            .files
            .get_mut("package.json")
            .expect("stub package.json");
        let mut pkg: serde_json::Value = serde_json::from_str(manifest).expect("package.json");
        pkg["dependencies"]["carrick-absent-dependency"] =
            serde_json::json!("file:./does-not-exist");
        *manifest = pkg.to_string();
    }

    /// The pair the install stopped is unverifiable, and says why in the
    /// installer's own words with no scratch path.
    fn assert_reads_the_installers_words(outcome: &PairCheckOutcome) {
        assert_eq!(outcome.bucket, VerdictBucket::Unverifiable, "{outcome:?}");
        let reason = outcome.unresolved_reason.as_deref().unwrap_or("");
        assert!(
            reason.contains("workspace dependency install failed")
                && reason.contains("ERR_PNPM_LINKED_PKG_DIR_NOT_FOUND"),
            "the reason must carry the installer's output: {reason}"
        );
        assert!(!reason.contains("no detail"), "reason: {reason}");
        assert!(
            !reason.contains("/tmp/") && !reason.contains("/private/"),
            "reason leaked a scratch path: {reason}"
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
                    printed_names: Vec::new(),
                    raw_text_read: false,
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

    /// carrick#1516, end to end against the real sidecar ($0, no model): a
    /// producer whose handler maps rows to `scope: 'specific' | 'all'`, which
    /// TypeScript infers as `scope: string`, and an untyped consumer that hands
    /// the response to a setter declaring the literal union.
    ///
    /// The inference reads the handler twice, the capture keeps both readings,
    /// the definitions pass publishes the unwidened one beside the expanded
    /// definition, and the retype files the pair as the producer's type being
    /// wider than what it sends. Without the reading the same pair is
    /// incompatible, which is what a real scan reported.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn producer_type_wider_than_its_handler_returns_is_its_own_class() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let fixture = manifest_dir.join("tests/fixtures/producer-wider");
        let api_root = fixture.join("api").canonicalize().expect("api fixture");
        let web_root = fixture.join("web").canonicalize().expect("web fixture");

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&api_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("GET", "/holidays");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let producer_infer = vec![InferRequestItem {
            file_path: api_root
                .join("src/routes.ts")
                .to_string_lossy()
                .into_owned(),
            line_number: 6,
            span_start: None,
            span_end: None,
            expression_text: None,
            expression_line: None,
            infer_kind: InferKind::ResponseBody,
            alias: Some(producer_alias.clone()),
            param_name: None,
        }];
        let inferred = sidecar
            .infer_types(&producer_infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        assert_eq!(inferred.len(), 1, "{inferred:#?}");
        assert!(
            inferred[0].type_string.contains("scope: string"),
            "{inferred:#?}"
        );
        let unwidened = inferred[0]
            .unwidened_type_string
            .clone()
            .unwrap_or_default();
        assert!(
            unwidened.contains("scope: \"all\" | \"specific\""),
            "{inferred:#?}"
        );

        let producer_anchors = derive_capture_anchors(
            &[],
            &producer_infer,
            &[],
            &inferred,
            std::slice::from_ref(&producer_alias),
            api_root.to_str().unwrap(),
        );
        let (api_stub, api_artifact) = run_capture(
            &sidecar,
            api_root.to_str().unwrap(),
            "api",
            &producer_anchors,
            &HashMap::new(),
            None,
        )
        .expect("api capture");
        let mut api = repo(
            "api",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                &producer_alias,
                "src/routes.ts",
                6,
                ManifestTypeState::Implicit,
            )],
            Some(api_artifact),
        );
        crate::engine::resolve_per_endpoint_definitions(&sidecar, &mut api, &api_stub);
        let _ = std::fs::remove_dir_all(&api_stub);
        let published = api.type_manifest.as_ref().unwrap()[0].clone();
        assert!(
            published
                .expanded_definition
                .as_deref()
                .is_some_and(|t| t.contains("scope: string")),
            "the published type is the compiler's inference: {published:#?}"
        );
        assert!(
            published
                .unwidened_definition
                .as_deref()
                .is_some_and(|t| t.contains("scope: \"all\" | \"specific\"")),
            "the handler's own return rides beside it: {published:#?}"
        );

        // The consumer, scanned in the same run.
        sidecar.start_init(&web_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("re-init");
        let site = crate::type_manifest::build_site_id(
            "src/holidays.ts",
            13,
            &key,
            web_root.to_str().unwrap(),
        );
        let consumer_alias = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&site),
        );
        let consumer_infer = vec![InferRequestItem {
            file_path: web_root
                .join("src/holidays.ts")
                .to_string_lossy()
                .into_owned(),
            line_number: 13,
            span_start: None,
            span_end: None,
            expression_text: Some("api.get(\"/holidays\")".to_string()),
            expression_line: Some(13),
            infer_kind: InferKind::CallResult,
            alias: Some(consumer_alias.clone()),
            param_name: None,
        }];
        let consumer_inferred = sidecar
            .infer_types(&consumer_infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        let consumer_anchors = derive_capture_anchors(
            &[],
            &consumer_infer,
            &[],
            &consumer_inferred,
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
        let web = repo(
            "web",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                &consumer_alias,
                "src/holidays.ts",
                13,
                ManifestTypeState::Unknown,
            )],
            Some(web_artifact),
        );
        let local = LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: web_root.clone(),
                tsconfig: None,
                calls: consumer_call_locators(&consumer_infer),
            },
        )]);

        let outcomes = run_check(&sidecar, &[api.clone(), web.clone()], &local);
        assert_eq!(outcomes.len(), 1, "{outcomes:#?}");
        let outcome = &outcomes[0];
        assert_eq!(outcome.bucket, VerdictBucket::ProducerWider, "{outcome:#?}");
        assert!(outcome.resolved, "{outcome:#?}");
        assert_eq!(outcome.diagnostic, None);
        let note = outcome
            .notes
            .iter()
            .find(|n| n.starts_with(PRODUCER_WIDER_NOTE))
            .unwrap_or_else(|| panic!("the drift is stated: {outcome:#?}"));
        assert!(
            note.contains("src/holidays.ts:14:") && note.contains("scope"),
            "the note names where the declared type fails: {note}"
        );

        // Control: the same pair with no reading published is a mismatch.
        let mut widened_only = api.clone();
        widened_only.type_manifest.as_mut().unwrap()[0].unwidened_definition = None;
        let control = run_check(&sidecar, &[widened_only, web], &local);
        assert_eq!(
            control[0].bucket,
            VerdictBucket::Incompatible,
            "{control:#?}"
        );
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
             console.log(body.x);\n\
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
                    printed_names: Vec::new(),
                    raw_text_read: false,
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

    /// carrick#1842: a consumer that reads the body with `res.text()` states
    /// no structural contract, end to end against the real sidecar ($0, no
    /// model). The inferrer marks the `string` it publishes, the mark rides
    /// the literal anchor into the stub's record, and the check reads the pair
    /// unverifiable rather than comparing `string` with the producer's object.
    /// The retype does not reopen it. Without the mark the same pair is the
    /// false incompatible the ticket reports.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_body_read_as_raw_text_is_not_judged_against_a_json_body() {
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
            web_root.join("src/ping.ts"),
            "export async function ping(): Promise<string> {\n  \
             const res = await fetch(\"/ping\");\n  \
             return res.text();\n\
             }\n",
        )
        .unwrap();
        let call_line = 2;

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&web_root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let key = OperationKey::http("POST", "/ping");
        let producer_alias =
            build_manifest_type_alias(&key, ManifestRole::Producer, ManifestTypeKind::Response);
        let site = crate::type_manifest::build_site_id(
            "src/ping.ts",
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
            file_path: web_root.join("src/ping.ts").to_string_lossy().into_owned(),
            line_number: call_line,
            span_start: None,
            span_end: None,
            expression_text: Some("fetch(\"/ping\")".to_string()),
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
        assert_eq!(inferred.len(), 1, "{inferred:#?}");
        assert_eq!(inferred[0].type_string.trim(), "string", "{inferred:#?}");
        assert!(inferred[0].raw_text_read, "{inferred:#?}");

        let (api_stub, api_artifact) = run_capture(
            &sidecar,
            api_root.to_str().unwrap(),
            "api",
            &[CaptureAnchor::Literal {
                alias: producer_alias.clone(),
                type_text: "{ ok: boolean }".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                source_file: None,
                printed_names: Vec::new(),
                raw_text_read: false,
            }],
            &HashMap::new(),
            None,
        )
        .expect("api capture");
        let _ = std::fs::remove_dir_all(&api_stub);
        let api = repo(
            "api",
            None,
            vec![entry(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                &producer_alias,
                "src/routes.ts",
                1,
                ManifestTypeState::Explicit,
            )],
            Some(api_artifact),
        );
        let web = |inferred: &[crate::services::type_sidecar::InferredType]| {
            let anchors = derive_capture_anchors(
                &[],
                &infer,
                &[],
                inferred,
                std::slice::from_ref(&consumer_alias),
                web_root.to_str().unwrap(),
            );
            let (dir, artifact) = run_capture(
                &sidecar,
                web_root.to_str().unwrap(),
                "web",
                &anchors,
                &HashMap::new(),
                None,
            )
            .expect("web capture");
            let _ = std::fs::remove_dir_all(&dir);
            repo(
                "web",
                None,
                vec![entry(
                    key.clone(),
                    ManifestRole::Consumer,
                    ManifestTypeKind::Response,
                    &consumer_alias,
                    "src/ping.ts",
                    call_line,
                    ManifestTypeState::Implicit,
                )],
                Some(artifact),
            )
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

        let text = only(run_check(&sidecar, &[api.clone(), web(&inferred)], &local));
        assert_eq!(text.bucket, VerdictBucket::Unverifiable, "{text:#?}");
        assert_eq!(text.gate.as_deref(), Some("consumer:text"), "{text:#?}");
        assert!(!text.resolved, "{text:#?}");

        // Control: the same read with no mark is compared, and reads as the
        // false incompatible the ticket reports.
        let mut unmarked = inferred.clone();
        unmarked[0].raw_text_read = false;
        let control = only(run_check(&sidecar, &[api, web(&unmarked)], &local));
        assert_eq!(control.bucket, VerdictBucket::Incompatible, "{control:#?}");
    }

    /// carrick#2054, end to end against the real sidecar ($0, no model): a
    /// route whose handler answers a different body for each value of a
    /// request field is judged, at each call, against the body the call's
    /// stated value selects. The producer's inference reads the modes, the
    /// capture publishes each case, the definitions pass writes the case
    /// texts onto the manifest, and the check probes (or the retype states)
    /// the case. A call that states no value reads unverifiable with the
    /// reason, never incompatible. A union no request read decides is judged
    /// as before.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_moded_response_is_judged_against_the_case_each_call_states() {
        use crate::services::type_sidecar::MessageSource;
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let tsconfig = r#"{"compilerOptions":{"strict":true,"target":"es2022","module":"esnext","moduleResolution":"bundler","lib":["es2022","dom"],"skipLibCheck":true},"include":["src"]}"#;
        let api_dir = tempfile::tempdir().unwrap();
        let api_root = api_dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(api_root.join("src")).unwrap();
        std::fs::write(api_root.join("tsconfig.json"), tsconfig).unwrap();
        // Line numbers are read off these texts; keep the two in step.
        std::fs::write(
            api_root.join("src/routes.ts"),
            "declare const flags: { beta: boolean };\n\
             export async function getItems(req: Request) {\n  \
             const mode = new URL(req.url).searchParams.get('mode');\n  \
             if (mode === 'a') return Response.json({ x: 1 });\n  \
             return Response.json({ y: 'z' });\n\
             }\n\
             export async function postItems(req: Request) {\n  \
             const { kind } = await req.json();\n  \
             switch (kind) {\n    \
             case 'p':\n    \
             case 'q':\n      \
             return Response.json({ p: true });\n    \
             case 'r':\n      \
             return Response.json({ r: 1 });\n    \
             default:\n      \
             return Response.json({ n: 0 });\n  \
             }\n\
             }\n\
             export async function getState() {\n  \
             if (flags.beta) return Response.json({ x: 1 });\n  \
             return Response.json({ y: 'z' });\n\
             }\n",
        )
        .unwrap();
        let web_dir = tempfile::tempdir().unwrap();
        let web_root = web_dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(web_root.join("src")).unwrap();
        std::fs::write(web_root.join("tsconfig.json"), tsconfig).unwrap();
        std::fs::write(
            web_root.join("src/client.ts"),
            "export async function typedOnA() {\n  \
             const res = await fetch(\"/api/items?mode=a\");\n  \
             return (await res.json()) as { x: number };\n\
             }\n\
             export async function readsYOnA() {\n  \
             const res = await fetch(\"/api/items?mode=a\");\n  \
             const body = await res.json();\n  \
             console.log(body.y);\n\
             }\n\
             export async function readsYOnB() {\n  \
             const res = await fetch(\"/api/items?mode=b\");\n  \
             const body = await res.json();\n  \
             console.log(body.y);\n\
             }\n\
             export async function readsXUnstated() {\n  \
             const res = await fetch(\"/api/items\");\n  \
             const body = await res.json();\n  \
             console.log(body.x);\n\
             }\n\
             export async function postsR() {\n  \
             const res = await fetch(\"/api/items\", { method: \"POST\", body: JSON.stringify({ kind: \"r\" }) });\n  \
             const body = await res.json();\n  \
             console.log(body.r);\n\
             }\n\
             export async function readsXOnState() {\n  \
             const res = await fetch(\"/api/state\");\n  \
             const body = await res.json();\n  \
             console.log(body.x);\n\
             }\n",
        )
        .unwrap();

        // ---- the producer: inferred, captured, its definitions resolved ----
        let api_sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        api_sidecar.start_init(&api_root, None);
        api_sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("api init");
        let items = OperationKey::http("GET", "/api/items");
        let post_items = OperationKey::http("POST", "/api/items");
        let state = OperationKey::http("GET", "/api/state");
        let routes = [(&items, 2u32), (&post_items, 7), (&state, 20)];
        let producer_alias = |key: &OperationKey| {
            build_manifest_type_alias(key, ManifestRole::Producer, ManifestTypeKind::Response)
        };
        let routes_file = api_root
            .join("src/routes.ts")
            .to_string_lossy()
            .into_owned();
        let api_infer: Vec<InferRequestItem> = routes
            .iter()
            .map(|(key, line)| InferRequestItem {
                file_path: routes_file.clone(),
                line_number: *line,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::ResponseBody,
                alias: Some(producer_alias(key)),
                param_name: None,
            })
            .collect();
        let api_inferred = api_sidecar
            .infer_types(&api_infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        let modes_of = |key: &OperationKey| {
            api_inferred
                .iter()
                .find(|inf| inf.alias == producer_alias(key))
                .and_then(|inf| inf.response_modes.clone())
        };
        assert!(modes_of(&items).is_some(), "{api_inferred:#?}");
        assert!(modes_of(&post_items).is_some(), "{api_inferred:#?}");
        assert_eq!(
            modes_of(&state),
            None,
            "a server-state branch reads no mode"
        );

        let mut api_manifest: Vec<TypeManifestEntry> = routes
            .iter()
            .map(|(key, line)| {
                entry(
                    (*key).clone(),
                    ManifestRole::Producer,
                    ManifestTypeKind::Response,
                    &producer_alias(key),
                    "src/routes.ts",
                    *line,
                    ManifestTypeState::Unknown,
                )
            })
            .collect();
        let api_aliases: Vec<String> = api_manifest.iter().map(|e| e.type_alias.clone()).collect();
        let anchors = derive_capture_anchors(
            &[],
            &api_infer,
            &[],
            &api_inferred,
            &api_aliases,
            api_root.to_str().unwrap(),
        );
        crate::engine::stamp_response_modes(&mut api_manifest, &api_inferred);
        let (api_stub, api_artifact) = run_capture(
            &api_sidecar,
            api_root.to_str().unwrap(),
            "api",
            &anchors,
            &HashMap::new(),
            None,
        )
        .expect("api capture");
        let records = crate::engine::read_capture_records(&api_stub);
        let aliases = crate::engine::aliases_to_resolve(&api_manifest, &records);
        let resolved = api_sidecar
            .resolve_definitions(api_stub.to_str().unwrap(), &aliases)
            .expect("definitions");
        crate::engine::apply_resolved_definitions(&mut api_manifest, resolved, &records);
        let _ = std::fs::remove_dir_all(&api_stub);
        let items_modes = api_manifest[0].response_modes.clone().expect("modes");
        assert!(
            items_modes.cases.iter().all(|case| case.expanded.is_some()),
            "every case is published: {items_modes:#?}"
        );
        assert_eq!(api_manifest[2].response_modes, None);
        drop(api_sidecar);

        // ---- the consumer: one call per row of the ticket's table ----
        let web_sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        web_sidecar.start_init(&web_root, None);
        web_sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("web init");
        // (operation, line, call expression, body literals)
        type Call<'a> = (&'a OperationKey, u32, &'a str, &'a [(&'a str, &'a str)]);
        let calls: [Call; 6] = [
            (&items, 2, "fetch(\"/api/items?mode=a\")", &[]),
            (&items, 6, "fetch(\"/api/items?mode=a\")", &[]),
            (&items, 11, "fetch(\"/api/items?mode=b\")", &[]),
            (&items, 16, "fetch(\"/api/items\")", &[]),
            (
                &post_items,
                21,
                "fetch(\"/api/items\", { method: \"POST\", body: JSON.stringify({ kind: \"r\" }) })",
                &[("kind", "r")],
            ),
            (&state, 26, "fetch(\"/api/state\")", &[]),
        ];
        let client_file = web_root
            .join("src/client.ts")
            .to_string_lossy()
            .into_owned();
        let consumer_alias = |key: &OperationKey, line: u32| {
            let site = crate::type_manifest::build_site_id(
                "src/client.ts",
                line,
                key,
                web_root.to_str().unwrap(),
            );
            build_manifest_type_alias_with_site_id(
                key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                Some(&site),
            )
        };
        let web_infer: Vec<InferRequestItem> = calls
            .iter()
            .map(|(key, line, text, _)| InferRequestItem {
                file_path: client_file.clone(),
                line_number: *line,
                span_start: None,
                span_end: None,
                expression_text: Some((*text).to_string()),
                expression_line: Some(*line),
                infer_kind: InferKind::CallResult,
                alias: Some(consumer_alias(key, *line)),
                param_name: None,
            })
            .collect();
        let web_inferred = web_sidecar
            .infer_types(&web_infer, None)
            .expect("infer")
            .inferred_types
            .unwrap_or_default();
        let web_aliases: Vec<String> = web_infer.iter().filter_map(|i| i.alias.clone()).collect();
        let web_anchors = derive_capture_anchors(
            &[],
            &web_infer,
            &[],
            &web_inferred,
            &web_aliases,
            web_root.to_str().unwrap(),
        );
        let (web_stub, web_artifact) = run_capture(
            &web_sidecar,
            web_root.to_str().unwrap(),
            "web",
            &web_anchors,
            &HashMap::new(),
            None,
        )
        .expect("web capture");
        let _ = std::fs::remove_dir_all(&web_stub);
        let web_manifest: Vec<TypeManifestEntry> = calls
            .iter()
            .map(|(key, line, text, body)| {
                let mut consumer = entry(
                    (*key).clone(),
                    ManifestRole::Consumer,
                    ManifestTypeKind::Response,
                    &consumer_alias(key, *line),
                    "src/client.ts",
                    *line,
                    ManifestTypeState::Implicit,
                );
                let target = text
                    .trim_start_matches("fetch(\"")
                    .split('"')
                    .next()
                    .unwrap_or_default();
                let query = crate::engine::stated_query(target);
                if !query.is_empty() {
                    consumer.stated_values.insert(MessageSource::Query, query);
                }
                if !body.is_empty() {
                    consumer.stated_values.insert(
                        MessageSource::Body,
                        body.iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    );
                }
                consumer
            })
            .collect();
        let local = LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: web_root.clone(),
                tsconfig: None,
                calls: consumer_call_locators(&web_infer),
            },
        )]);
        let repos = [
            repo("api", None, api_manifest, Some(api_artifact)),
            repo("web", None, web_manifest, Some(web_artifact)),
        ];
        let outcomes = run_check(&web_sidecar, &repos, &local);
        let at = |line: u32| -> &PairCheckOutcome {
            outcomes
                .iter()
                .find(|o| o.consumer_line == line)
                .unwrap_or_else(|| panic!("no outcome at line {line}: {outcomes:#?}"))
        };
        let verdicts: Vec<(u32, VerdictBucket, Option<&str>)> = calls
            .iter()
            .map(|(_, line, _, _)| (*line, at(*line).bucket, at(*line).gate.as_deref()))
            .collect();
        eprintln!("{verdicts:#?}");

        // `?mode=a`, typed `{ x: number }`: the check probes the case.
        assert_eq!(at(2).bucket, VerdictBucket::Compatible, "{:#?}", at(2));
        // `?mode=a`, reads `y`: a real break on a stated case is kept.
        assert_eq!(at(6).bucket, VerdictBucket::Incompatible, "{:#?}", at(6));
        // `?mode=b`, reads `y`: the case for any other value.
        assert_eq!(at(11).bucket, VerdictBucket::Compatible, "{:#?}", at(11));
        // No `mode`, reads `x`: not compared, with the reason.
        let unstated = at(16);
        assert_eq!(
            unstated.bucket,
            VerdictBucket::Unverifiable,
            "{unstated:#?}"
        );
        assert_eq!(unstated.gate.as_deref(), Some("producer:mode"));
        assert_eq!(
            unstated.unresolved_reason.as_deref(),
            Some(
                "Not compared: this route returns a different response for each value of `mode` in the request (`a`), and this call does not set `mode` to one of them."
            )
        );
        // Body `kind: 'r'`, reads `r`: the body field's case.
        assert_eq!(at(21).bucket, VerdictBucket::Compatible, "{:#?}", at(21));
        // Control: a union no request read decides reads as it did.
        assert_eq!(at(26).bucket, VerdictBucket::Incompatible, "{:#?}", at(26));
    }

    /// carrick#1980: a subscriber the inference left unanswered is captured
    /// by the parameter its request names, end to end against the real
    /// sidecar ($0, no model): the request this client writes, read by the
    /// sidecar process, answered by its capture.
    ///
    /// The anchor sits on a line inside the handler, where a comparison is
    /// the first expression. The sidecar's request schema used to drop the
    /// parameter name, so the capture typed that line and published
    /// `boolean` as what the subscriber receives, self-checked clean. A name
    /// no parameter has was typed the same way where it must abstain.
    ///
    /// No inference is handed to the derivation: the inferrer looks for the
    /// handler within two lines of the anchor and answers nothing this far
    /// into its body, which is the case that reaches the capture as a
    /// located anchor rather than as printed text.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_subscriber_anchor_is_captured_by_the_parameter_it_names() {
        let sidecar_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
        if !sidecar_path.exists() {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true,"target":"es2022","module":"esnext","moduleResolution":"bundler","skipLibCheck":true},"include":["src"]}"#,
        )
        .unwrap();
        // Line numbers are read off this text; keep the two in step.
        std::fs::write(
            root.join("src/listener.ts"),
            "export interface PreviewMessage {\n  \
             origin: string;\n  \
             data?: { type: string };\n\
             }\n\
             \n\
             declare const expectedOrigin: string;\n\
             declare function setReady(ready: boolean): void;\n\
             \n\
             export function listener(message: PreviewMessage) {\n  \
             if (message.origin !== expectedOrigin) {\n    \
             return;\n  \
             }\n\
             \n  \
             if (message?.data?.type === 'ready') {\n    \
             setReady(true);\n  \
             }\n\
             }\n",
        )
        .unwrap();
        let comparison_line = 14;

        let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
        sidecar.start_init(&root, None);
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .expect("sidecar init");

        let (named, unnamed) = ("Named_Producer_Response", "Unnamed_Producer_Response");
        let request = |alias: &str, param_name: &str| InferRequestItem {
            file_path: root.join("src/listener.ts").to_string_lossy().into_owned(),
            line_number: comparison_line,
            span_start: None,
            span_end: None,
            expression_text: None,
            expression_line: None,
            infer_kind: InferKind::FunctionParam,
            alias: Some(alias.to_string()),
            param_name: Some(param_name.to_string()),
        };
        let infer = vec![request(named, "message"), request(unnamed, "message.data")];
        let anchors = derive_capture_anchors(
            &[],
            &infer,
            &[],
            &[],
            &[named.to_string(), unnamed.to_string()],
            root.to_str().unwrap(),
        );
        for (anchor, param) in anchors.iter().zip(["message", "message.data"]) {
            assert!(
                matches!(
                    anchor,
                    CaptureAnchor::Infer { param_name: Some(name), .. } if name == param
                ),
                "the request's parameter name rides its anchor: {anchor:#?}"
            );
        }

        let (stub, artifact) = run_capture(
            &sidecar,
            root.to_str().unwrap(),
            "listener",
            &anchors,
            &HashMap::new(),
            None,
        )
        .expect("capture");
        let _ = std::fs::remove_dir_all(&stub);
        let surface = artifact.files.get("types/surface.d.ts").unwrap();
        let published = |alias: &str| {
            surface
                .lines()
                .find_map(|line| line.strip_prefix(&format!("export type {alias} = ")))
                .unwrap_or_else(|| panic!("no surface line for {alias}: {surface}"))
        };
        assert!(
            published(named).contains("PreviewMessage"),
            "the handler's parameter is the subscriber's type: {surface}"
        );
        assert_eq!(
            published(unnamed),
            "unknown;",
            "a name no parameter has abstains: {surface}"
        );
        assert!(
            !surface.contains("boolean"),
            "neither is the comparison on the anchor's line: {surface}"
        );
        let records = artifact.files.get("carrick-manifest.json").unwrap();
        assert!(
            records.contains("no handler parameter 'message.data' resolved"),
            "the abstention says which name resolved nothing: {records}"
        );
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
            consumer_response: false,
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
                consumer_response: false,
            },
            CaptureAnchor::Symbol {
                alias: "Pub_OrderPlacedEvent".to_string(),
                symbol_name: "OrderPlacedEvent".to_string(),
                source_file: "src/types/events.ts".to_string(),
                anchor_origin: AnchorOrigin::LlmSymbol,
                array_depth: None,
                consumer_response: false,
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
                consumer_response: false,
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
                printed_names: Vec::new(),
                raw_text_read: false,
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

    /// carrick#2021: a retype the time limit cut short is counted, in the
    /// scan summary and in the consumer's boundary on the index, and a retype
    /// that finished counts nothing.
    ///
    /// Three consumer calls of `web` go to a stand-in sidecar that answers the
    /// way the real one does when its time limit passes before it reaches an
    /// item (`retype.ts`); the control stand-in judges every item. The check
    /// runs as a scan runs it, its outcomes are folded into the boundary as
    /// the engine folds them, and the summary is the run's total.
    #[test]
    fn a_retype_the_time_limit_cut_short_is_counted_in_the_summary_and_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let stand_in = |name: &str, answer: &str| -> TypeSidecar {
            let script = root.join(name);
            std::fs::write(
                &script,
                format!(
                    r#"
const fs = require('fs');
const write = (frame) => fs.writeSync(1, JSON.stringify(frame) + '\n');
require('readline').createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  const request_id = request.request_id;
  if (request.action === 'shutdown') process.exit(0);
  if (request.action === 'init') return write({{ request_id, status: 'ready' }});
  if (request.action === 'list_program_files') return write({{ request_id, status: 'success', files: [] }});
  if (request.action === 'add_program_files') return write({{ request_id, status: 'success', added: request.files.length }});
  write({{
    request_id,
    status: 'success',
    outcomes: request.items.map((item) => ({{ item_id: item.item_id, diagnostics: [], {answer} }})),
  }});
}});
"#
                ),
            )
            .unwrap();
            let sidecar = TypeSidecar::spawn(&script).unwrap();
            sidecar.start_init(&root, None);
            sidecar
                .wait_ready(std::time::Duration::from_secs(20))
                .expect("the stand-in answers init");
            sidecar
        };

        let key = OperationKey::http("GET", "/orders");
        let mut producer = entry(
            key.clone(),
            ManifestRole::Producer,
            ManifestTypeKind::Response,
            "Orders",
            "src/routes.ts",
            3,
            ManifestTypeState::Explicit,
        );
        producer.expanded_definition = Some("{ id: string; }".to_string());
        let calls: Vec<TypeManifestEntry> = [8, 9, 10]
            .into_iter()
            .map(|line| {
                entry(
                    key.clone(),
                    ManifestRole::Consumer,
                    ManifestTypeKind::Response,
                    &format!("Call{line}"),
                    "src/client.ts",
                    line,
                    ManifestTypeState::Unknown,
                )
            })
            .collect();
        let local = LocalConsumers::from([(
            "web".to_string(),
            LocalConsumer {
                root: root.clone(),
                tsconfig: None,
                calls: calls
                    .iter()
                    .map(|call| {
                        (
                            call.type_alias.clone(),
                            CallLocator {
                                file_path: root.join("src/client.ts").display().to_string(),
                                line_number: call.line_number,
                                span_start: None,
                                span_end: None,
                                expression_text: None,
                                expression_line: None,
                            },
                        )
                    })
                    .collect(),
            },
        )]);
        // The consumer has no surface of its own, so every pair is its to
        // settle and only the retype can.
        let all_repo_data = [
            repo("api", None, vec![producer], Some(fake_artifact())),
            repo("web", None, calls, None),
        ];
        let scanned = |sidecar: &TypeSidecar| {
            let outcomes = run_check(sidecar, &all_repo_data, &local);
            let mut boundaries = vec![(
                "web".to_string(),
                crate::boundary::ServiceBoundary::default(),
            )];
            let summary = crate::time_limits::fold_check_outcomes(&mut boundaries, &outcomes);
            let index = serde_json::to_value(&boundaries[0].1).unwrap();
            (summary.lines(), index["time_limits_run_out"].clone())
        };

        let cut_short = stand_in(
            "cut-short.cjs",
            "outcome: 'abstain', reason: 'the retype check ran out of its 600000ms budget'",
        );
        assert_eq!(
            scanned(&cut_short),
            (
                vec!["3 response checks not reached: retype time limit".to_string()],
                serde_json::json!({ "retype": 3 })
            )
        );

        let finished = stand_in("finished.cjs", "outcome: 'agrees'");
        assert_eq!(scanned(&finished), (Vec::new(), serde_json::json!({})));
    }
}
