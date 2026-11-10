//! Typed findings — the scanner's single model for everything a scan
//! surfaces, serialized verbatim as the `post-pr-result` wire payload
//! (schema_version 1, carrick-cloud docs/internal/pr-result-pipeline.md)
//! and rendered locally by the formatter. The types here are the schema
//! source of truth: the cloud renders the PR comment/check run from this
//! JSON and never re-parses prose.

use crate::operation::EndpointProvenance;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};

/// Wire cap on `call_sites` entries per finding. Applied when serializing
/// only — the in-memory finding keeps the full set so the terminal report
/// shows true counts.
pub const MAX_CALL_SITES: usize = 5;
/// Wire cap on `detail` length in *chars* (applied at construction).
pub const MAX_DETAIL_CHARS: usize = 400;

/// Truncate `s` to at most `max` chars, appending `"..."` when cut. Operates
/// on char boundaries — never byte-slices — replacing the old `&s[..150]`
/// truncation that panicked mid-UTF-8 sequence. The result never exceeds
/// `max` chars: when `max <= 3` leaves no room for the ellipsis the string
/// is hard-cut instead, so the ≤ max contract holds for every input.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

/// Write a mismatch's comparison with main (carrick-cloud#1369, #1408).
///
/// `on_main` is written when the PR run could tell, `on_main_unknown` when it
/// could not, and neither on a run that is not a PR run, so such a payload is
/// exactly what it was before either field existed. The cloud reads only
/// `on_main: false` as introduced by the PR.
fn serialize_on_main<M: SerializeMap>(
    map: &mut M,
    on_main: &Option<bool>,
    on_main_unknown: &Option<OnMainUnknown>,
) -> Result<(), M::Error> {
    match (on_main, on_main_unknown) {
        (Some(on_main), _) => map.serialize_entry("on_main", on_main),
        (None, Some(reason)) => map.serialize_entry("on_main_unknown", reason),
        (None, None) => Ok(()),
    }
}

/// The wire view of a `call_sites` list: at most [`MAX_CALL_SITES`] entries.
fn wire_call_sites(call_sites: &[String]) -> &[String] {
    &call_sites[..call_sites.len().min(MAX_CALL_SITES)]
}

/// Fixed per finding kind (see [`Finding::severity`]); sent explicitly on the
/// wire so the cloud never re-derives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Risk,
    Gap,
    Advisory,
}

/// Which layer the rows behind a finding's edge come from (carrick#705,
/// cloud#599). A finding is a `Fact` only when EVERY row that contributes to
/// it — the producer endpoint and every consumer call site — is one the source
/// states outright; a single row whose only source is the model makes the whole
/// pairing a `Candidate`, because a pairing is no better than its weakest side.
/// So does a consumer call site the scan holds no row for (carrick#1735): a
/// pairing with a side the scan cannot see is not a fact.
///
/// Absent on the wire when every row is held but one states no source (see
/// [`EdgeSource::fold`]), which readers take as "not stated", never
/// "candidate"; the cloud's PR check enforces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeSource {
    Fact,
    Candidate,
}

impl EdgeSource {
    /// Fold the contributing rows' [`ResolutionSource`]s into one verdict.
    ///
    /// [`ResolutionSource`]: crate::agents::file_analyzer_agent::ResolutionSource
    ///
    /// - any row whose source is a candidate
    ///   ([`ResolutionSource::is_candidate`]: the model stated it) ->
    ///   `Candidate`, a positive statement;
    /// - otherwise, every row stated by a deterministic pass (and at least one
    ///   row) -> `Fact`;
    /// - otherwise `None`: a row that recorded no source at all (an older
    ///   peer blob, or a row built outside the emit/join phase) leaves
    ///   the fold unable to claim `Fact`, and claiming `Candidate` off a
    ///   silence would demote deterministic rows that simply do not carry the
    ///   field.
    ///
    /// [`ResolutionSource::is_candidate`]: crate::agents::file_analyzer_agent::ResolutionSource::is_candidate
    pub fn fold(
        sources: impl IntoIterator<Item = Option<crate::agents::file_analyzer_agent::ResolutionSource>>,
    ) -> Option<Self> {
        let mut any_row = false;
        let mut all_stated = true;
        for source in sources {
            any_row = true;
            match source {
                Some(source) if source.is_candidate() => {
                    return Some(EdgeSource::Candidate);
                }
                Some(_) => {}
                None => all_stated = false,
            }
        }
        (any_row && all_stated).then_some(EdgeSource::Fact)
    }

    /// Two folds of one edge into one, by [`EdgeSource::fold`]'s rule:
    /// `Candidate` wins, then unstated, else `Fact`. For an edge several
    /// outcomes (or several matches on one call site) were filed on.
    pub fn combine(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(EdgeSource::Candidate), _) | (_, Some(EdgeSource::Candidate)) => {
                Some(EdgeSource::Candidate)
            }
            (Some(EdgeSource::Fact), Some(EdgeSource::Fact)) => Some(EdgeSource::Fact),
            _ => None,
        }
    }
}

/// How far the type layer got on a finding's pair (carrick#705, cloud#599).
///
/// `Resolved` is the compiler's answer on two fully-resolved sides: neither
/// carries `any`, `unknown` or an unresolved reference at any depth. `NotChecked`
/// says no verdict APPLIES to this finding kind (a wrong verb on a declared
/// route is a fact about routing, not about types) and is deliberately
/// distinct from `Unresolved`, which says a verdict was attempted and could
/// not be reached. A reader that enforces only on facts must treat
/// `NotChecked` as enforceable and `Unresolved` as not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictState {
    Resolved,
    Unresolved,
    NotChecked,
}

/// Why a PR run could not say whether main already had a finding
/// (carrick-cloud#1408). Sent as `on_main_unknown` in place of `on_main`, so
/// the cloud can say it could not tell rather than read the finding as one
/// the PR introduced.
///
/// `on_main: false` is only ever sent when main's side of the comparison is
/// main as this PR's base has it, judged the same way this run judged the PR.
/// Every other case is one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnMainUnknown {
    /// This repo has no index on main to compare with.
    NoMainIndex,
    /// Main's index is not of this PR's base: it was scanned at a commit this
    /// clone shows differs from the base (main's own scan may still be
    /// running), or from a working tree with uncommitted changes.
    MainIndexStale,
    /// Another scanner version wrote main's index, and nothing shows it reads
    /// code as this run does: its rows for a file the PR left alone differ
    /// from this run's, or the PR left no such file to compare them on
    /// (carrick#1530). Its extraction or type check may differ from this
    /// run's, so a finding it lacks may not be the PR's. Separate from
    /// [`OnMainUnknown::MainIndexStale`] so the cloud can treat it differently
    /// without a scanner release.
    MainIndexOtherScanner,
    /// Main's side of the comparison failed, panicked or ran out of time.
    MainSideFailed,
    /// Main's index holds no type verdict this run can compare this pairing
    /// with. A verdict the PR run reached by reading the consumer's own source
    /// cannot be reached again from main's stored index, so main's stored
    /// verdict is the only answer, and it is missing or unresolved.
    MainTypesUnjudged,
    /// A pairing main had was lost here, but the run cannot show that this
    /// PR's diff removed the side that is gone (carrick#3092): the removal is
    /// not placed in the diff and the removed side's service is not provably
    /// untouched, or the two contradict each other. Doubt lands here, never on
    /// `on_main: false`.
    RemovalUnplaced,
    /// The pairing exists only at the PR's head, but the PR changed an input
    /// other than source files that the producer's or consumer's service
    /// reads (its manifest, lockfile, tsconfig chain or `carrick.json`,
    /// carrick#1712). Those can move how the scanner reads code the PR never
    /// touched, so a pairing that only head shows may be the same code read
    /// differently, not one the PR wrote.
    ReadingMoved,
}

/// The operation a PR removed or renamed away that another service still uses
/// (carrick#3092): main's spelling of it and where main had it. `service` is
/// the service that served (or sent) it; `place` is `file:line` on main.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemovedSide {
    pub method: String,
    pub path: String,
    pub service: Option<String>,
    pub place: String,
}

/// A `missing_endpoint` or `orphaned_endpoint` row that records a pairing main
/// had and this PR run lost (carrick#3092). Present only on a PR run and only
/// on a row that lost a pairing; its presence makes the row a `risk`. Every
/// other row of both kinds serializes exactly as before.
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    pub removed: RemovedSide,
    /// Whether every row behind the pairing is one the source states.
    pub edge_source: Option<EdgeSource>,
    /// `NotChecked`: a lost pairing is a routing fact, not a type verdict.
    pub verdict_state: Option<VerdictState>,
    pub on_main: Option<bool>,
    /// Why `on_main` is `None`. Never set alongside `on_main`.
    pub on_main_unknown: Option<OnMainUnknown>,
}

/// Write a removal's fields beside the row's own (carrick#3092).
fn serialize_removal<M: SerializeMap>(map: &mut M, removal: &Removal) -> Result<(), M::Error> {
    map.serialize_entry("removed", &removal.removed)?;
    if let Some(edge_source) = &removal.edge_source {
        map.serialize_entry("edge_source", edge_source)?;
    }
    if let Some(verdict_state) = &removal.verdict_state {
        map.serialize_entry("verdict_state", verdict_state)?;
    }
    serialize_on_main(map, &removal.on_main, &removal.on_main_unknown)
}

/// Whether a finding is a break, and how sure the scanner is (the one break
/// answer; plan R2). Every surface that decides "does this fail / post / count"
/// reads this instead of folding `edge_source`, `verdict_state` and `on_main`
/// itself. A pure function of the finding, written on the wire as `break` and
/// `break_reason` (see [`BreakStatus::wire_label`]); the PR comment, MCP and
/// dashboard still carry their own copies until they read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakStatus {
    /// Proven and introduced by the change: `fact` edge (or an SDK-mediated
    /// one, which states no source by design and is enforced), a verdict that
    /// was reached or needs none, and main did not have it.
    Certain,
    /// A fact the run could not place against main (`on_main_unknown`, or no
    /// comparison was made).
    Possible { reason: &'static str },
    /// Not proven: a row the model stated, an unresolved type side (R4), or a
    /// removal row with no source or main marking behind it.
    Candidate { reason: &'static str },
    /// Main already had it.
    OnMain,
    /// Not a break at all (a gap, an advisory, a dependency conflict).
    NotABreak,
}

impl BreakStatus {
    /// The `break` value on the wire: `certain`, `possible`, `candidate`,
    /// `on_main` or `none`. Flat strings, not a derived enum, so the payload
    /// carries one value and its reason beside it (`break_reason`).
    pub fn wire_label(&self) -> &'static str {
        match self {
            BreakStatus::Certain => "certain",
            BreakStatus::Possible { .. } => "possible",
            BreakStatus::Candidate { .. } => "candidate",
            BreakStatus::OnMain => "on_main",
            BreakStatus::NotABreak => "none",
        }
    }

    /// Why a `possible` or `candidate` status is not `certain`; `None` for
    /// every other status. Written as `break_reason`, omitted when `None`.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            BreakStatus::Possible { reason } | BreakStatus::Candidate { reason } => Some(reason),
            BreakStatus::Certain | BreakStatus::OnMain | BreakStatus::NotABreak => None,
        }
    }
}

impl OnMainUnknown {
    fn reason(self) -> &'static str {
        match self {
            OnMainUnknown::NoMainIndex => "no_main_index",
            OnMainUnknown::MainIndexStale => "main_index_stale",
            OnMainUnknown::MainIndexOtherScanner => "main_index_other_scanner",
            OnMainUnknown::MainSideFailed => "main_side_failed",
            OnMainUnknown::MainTypesUnjudged => "main_types_unjudged",
            OnMainUnknown::RemovalUnplaced => "removal_unplaced",
            OnMainUnknown::ReadingMoved => "reading_moved",
        }
    }
}

/// The break answer for a type or method mismatch, from its inputs. Shared by
/// [`Finding::break_status`] and the local check, which holds the same inputs
/// as strings read off the index and derives `on_main` from its own "before"
/// instead of a baseline (so it passes that, and decides nothing itself).
///
/// An absent `edge_source` is the SDK-mediated case (no source by design,
/// enforced today at `render_pr_comment.ts` `isCandidateRisk`); a caller that
/// must not enforce an unstated source passes `Some(EdgeSource::Candidate)`.
pub fn mismatch_break_status(
    edge_source: Option<EdgeSource>,
    verdict_state: Option<VerdictState>,
    on_main: Option<bool>,
    on_main_unknown: Option<OnMainUnknown>,
) -> BreakStatus {
    if edge_source == Some(EdgeSource::Candidate) {
        return BreakStatus::Candidate {
            reason: "rests_on_a_model_stated_row",
        };
    }
    if verdict_state == Some(VerdictState::Unresolved) {
        return BreakStatus::Candidate {
            reason: "type_side_unresolved",
        };
    }
    match (on_main, on_main_unknown) {
        (Some(true), _) => BreakStatus::OnMain,
        (Some(false), _) => BreakStatus::Certain,
        (None, Some(why)) => BreakStatus::Possible {
            reason: why.reason(),
        },
        (None, None) => BreakStatus::Possible {
            reason: "not_compared_with_main",
        },
    }
}

/// One repo's pinned version of a conflicting package.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PackageVersionRef {
    pub repo: String,
    pub version: String,
    pub source: String,
    /// The specifier as written (`^4.0.0`), which `version` has lost the
    /// operators of. Omitted when the stored copy carries none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub spec: String,
}

/// One service of this repo whose written specifier for a conflicting package
/// differs between main's copy and the PR (carrick#2283). A side is `null`
/// where the service does not declare the package there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpecChange {
    pub service: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Dependency-conflict tier: `"major"` (semver-incompatible major spread,
/// counts as a gap) or `"unparseable"` (non-semver pins that differ as raw
/// strings, advisory).
pub mod tier {
    pub const MAJOR: &str = "major";
    pub const UNPARSEABLE: &str = "unparseable";
}

/// A single scan finding. Serializes as a `kind`-tagged union with an
/// explicit `severity` field (see the custom [`Serialize`] impl below).
/// `method`/`path` are protocol-agnostic display labels (HTTP verb + route,
/// GraphQL kind + field, socket direction + event); `call_sites` entries are
/// `file:line` (or bare `file`), capped at [`MAX_CALL_SITES`] on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// Producer and consumer types for one endpoint are incompatible.
    TypeMismatch {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
        producer_type: String,
        /// Empty when the consumer states no type, and then `consumer_reads`
        /// says what it was judged on.
        consumer_type: String,
        /// Compiler error, pre-truncated to [`MAX_DETAIL_CHARS`] chars.
        detail: String,
        /// Where the consumer reads what the producer's response does not
        /// provide, as `file:line`, when the retype check judged the pair from
        /// the consumer's own code (carrick#1491). That consumer states no
        /// type, so a reader shows these reads as its side of the pair
        /// (carrick#1517). Empty on every other mismatch.
        ///
        /// Scan-local, as `direction` is: the cloud reads the same places out
        /// of `detail`, so the payload does not carry them twice.
        consumer_reads: Vec<String>,
        /// Whether the producer shape comes from a real route or a mock/test
        /// handler (#380) — a mismatch against a mock is often still real
        /// (mocks frequently encode the canonical contract) but should be
        /// presented with that caveat.
        producer_provenance: EndpointProvenance,
        /// Whether every row behind this pairing is one the source states
        /// (cloud#599). `None` when the scan could not state it.
        edge_source: Option<EdgeSource>,
        /// How far the type layer got on this pair (cloud#599).
        verdict_state: Option<VerdictState>,
        /// Which half of the contract this mismatch is about (carrick#1033):
        /// the request the consumer sends, or the response the producer
        /// returns. Carried from the pair outcome's own `type_kind`, so
        /// nothing downstream has to read it back out of an alias name.
        ///
        /// `None` on a finding raised without a direction behind it — the
        /// SDK-mediated path, whose `producer_type`/`consumer_type` are side
        /// LABELS and not types at all ([`crate::sdk_edges`]). A reader that
        /// pairs the two type strings with a direction must therefore require
        /// this field, never assume a default.
        ///
        /// Scan-local: the hand-rolled [`Serialize`] below deliberately
        /// does NOT write it, because the local join reads the in-process
        /// struct and publishing a field on the PR-result payload the cloud
        /// reads is a payload-contract question rather than a scanner-internal
        /// one. Adding it there is a seam change, made on purpose or not at
        /// all.
        direction: Option<crate::cloud_storage::ManifestTypeKind>,
        /// The pairing this finding is about, as the scanner keys it
        /// (carrick-cloud#1369): producer service and operation, consumer
        /// service and file, and the direction. No line, because a PR that
        /// edits the consumer file above a call moves the line without
        /// changing the pairing. Scan-local: never written to the wire. The
        /// only reader is [`crate::pr_baseline`], which compares a PR run's
        /// findings with main's on it. `None` when the caller cannot state it.
        pair: Option<String>,
        /// Whether main's copy of this repo, matched against the same peers,
        /// yields this same finding (carrick-cloud#1369). Set only on a PR run;
        /// `None` when the run could not tell, and then `on_main_unknown` says
        /// why, and on every run that is not a PR run.
        on_main: Option<bool>,
        /// Why `on_main` is `None` on a PR run (carrick-cloud#1408). Never set
        /// alongside `on_main`.
        on_main_unknown: Option<OnMainUnknown>,
    },
    /// A consumer call matched a producer path but not its method. `method`
    /// is the consumer's attempt; `expected_method` is the producer's.
    MethodMismatch {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
        expected_method: String,
        /// Whether every row behind this pairing is one the source states
        /// (cloud#599). `None` when the scan could not state it.
        edge_source: Option<EdgeSource>,
        /// [`VerdictState::NotChecked`] whenever stated: a wrong verb on a
        /// declared route is a fact about routing, and no type verdict bears
        /// on it (cloud#599).
        verdict_state: Option<VerdictState>,
        /// The pairing this finding is about, as the scanner keys it
        /// (carrick-cloud#1369): producer service and operation, consumer
        /// service and file, and the direction. No line, because a PR that
        /// edits the consumer file above a call moves the line without
        /// changing the pairing. Scan-local: never written to the wire. The
        /// only reader is [`crate::pr_baseline`], which compares a PR run's
        /// findings with main's on it. `None` when the caller cannot state it.
        pair: Option<String>,
        /// Whether main's copy of this repo, matched against the same peers,
        /// yields this same finding (carrick-cloud#1369). Set only on a PR run;
        /// `None` when the run could not tell, and then `on_main_unknown` says
        /// why, and on every run that is not a PR run.
        on_main: Option<bool>,
        /// Why `on_main` is `None` on a PR run (carrick-cloud#1408). Never set
        /// alongside `on_main`.
        on_main_unknown: Option<OnMainUnknown>,
    },
    /// A consumer call with no producer in the index.
    MissingEndpoint {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
        /// Set when this row records a pairing main had and this PR lost
        /// (carrick#3092).
        removal: Option<Removal>,
    },
    /// A producer endpoint with no consumer in the index.
    OrphanedEndpoint {
        method: String,
        path: String,
        service: Option<String>,
        /// Whether this producer is a real route or a mock/test handler
        /// (#380). Orphaned mocks are expected (most mock handlers have no
        /// scanned consumer), so surfaces can de-noise on this.
        provenance: EndpointProvenance,
        /// Whether the module serving this route also renders a view
        /// (carrick#704/#713). A page's own data endpoint has no caller of its
        /// own by design, so "nothing calls it" is not an observation about it
        /// and the unconsumed section leaves it out (cloud#599).
        view_module: bool,
        /// The receiver's registration places, written only on a row that lost
        /// a pairing (carrick#3092); empty on every other row.
        sites: Vec<String>,
        /// Set when this row records a pairing main had and this PR lost
        /// (carrick#3092).
        removal: Option<Removal>,
    },
    /// A call whose URL is built from an env var not classified in
    /// carrick.json (`internalEnvVars` / `externalEnvVars`).
    EnvVarCall {
        method: String,
        path: String,
        env_var: String,
        call_sites: Vec<String>,
    },
    /// A handler that switches on a request field, and the operations behind
    /// it that nothing in `carrick.json` declares yet (carrick#831).
    ///
    /// Advisory, never a failure, and never a claim that anything is wrong:
    /// the handler works, the index simply has one operation where the source
    /// has several. What it carries is the material for the `operations`
    /// block a reader can paste — the field, every value, and the call sites
    /// that already name one — so accepting the model's reading is a copy
    /// rather than an authoring job (#357).
    ///
    /// `route` is nullable because the case this exists for is the ROUTELESS
    /// handler, whose route lives in infrastructure rather than in the source;
    /// when consumer calls name one, that is where it comes from.
    DispatchOperations {
        service: String,
        route: Option<String>,
        dispatch_location: String,
        dispatch_field: String,
        /// Every value the handler answers, in the order extraction stated
        /// them.
        values: Vec<String>,
        /// Call sites that already state a value for this field.
        call_sites: Vec<String>,
    },
    /// One package pinned to conflicting versions across repos. No
    /// method/path — this finding is not endpoint-scoped.
    DependencyConflict {
        package_name: String,
        /// [`tier::MAJOR`] or [`tier::UNPARSEABLE`].
        tier: String,
        versions: Vec<PackageVersionRef>,
        /// The services of this repo whose specifier the PR changed
        /// (carrick#2283). Empty on a run that is not a PR run, and when the
        /// PR left the package alone.
        changed_by_pr: Vec<SpecChange>,
        /// Whether main has this conflict at the same tier (carrick#2283).
        /// `None` on a run that is not a PR run, and when it could not tell.
        on_main: Option<bool>,
        /// Why a PR run could not tell. Never set beside `on_main`.
        on_main_unknown: Option<OnMainUnknown>,
    },
    /// Multiple repos encode the same external contract (#379): every side of
    /// the match is a call site, so no indexed service defines the route and
    /// producer/consumer roles do not apply. Advisory signal — "these N repos
    /// speak the same external API" — never a contract verdict.
    SharedExternalContract {
        method: String,
        path: String,
        /// Repo ids (service_name ?? repo_name) that encode this contract,
        /// sorted; always ≥2 (single-repo groups are not emitted).
        repos: Vec<String>,
        call_sites: Vec<String>,
    },
    /// Type extraction failed for one service, so every contract in it is
    /// unverified for this run (carrick#535). Not endpoint-scoped: it says
    /// the type layer is missing, which silently turns every type check into
    /// "not compared". A risk, because a green comment that omits it reads as
    /// "no type problems found" when the truth is "types were never read".
    DegradedTypes {
        /// Service this happened in (`service_name ?? repo_name`).
        service: String,
        /// Which stage lost the types: `spawn`, `init`, `resolve`, `capture`.
        stage: String,
        /// What went wrong, pre-truncated to [`MAX_DETAIL_CHARS`] chars.
        detail: String,
    },
}

impl Finding {
    pub fn type_mismatch(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
        producer_type: impl Into<String>,
        consumer_type: impl Into<String>,
        detail: &str,
    ) -> Self {
        Finding::TypeMismatch {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
            producer_type: producer_type.into(),
            consumer_type: consumer_type.into(),
            detail: truncate_chars(detail, MAX_DETAIL_CHARS),
            consumer_reads: Vec::new(),
            producer_provenance: EndpointProvenance::default(),
            edge_source: None,
            verdict_state: None,
            // Stated by the caller that knows the pair outcome's `type_kind`,
            // and by nobody else: a finding whose two type strings are side
            // labels must not read as a request or a response.
            direction: None,
            pair: None,
            on_main: None,
            on_main_unknown: None,
        }
    }

    pub fn method_mismatch(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
        expected_method: impl Into<String>,
    ) -> Self {
        Finding::MethodMismatch {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
            expected_method: expected_method.into(),
            edge_source: None,
            verdict_state: None,
            pair: None,
            on_main: None,
            on_main_unknown: None,
        }
    }

    pub fn missing_endpoint(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::MissingEndpoint {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
            removal: None,
        }
    }

    pub fn orphaned_endpoint(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
    ) -> Self {
        Finding::OrphanedEndpoint {
            method: method.into(),
            path: path.into(),
            service,
            provenance: EndpointProvenance::default(),
            view_module: false,
            sites: Vec::new(),
            removal: None,
        }
    }

    /// Record that this row is a pairing main had and this PR lost
    /// (carrick#3092), with the receiver's registration `sites` on an orphaned
    /// endpoint. No-op for every kind but the two endpoint rows.
    pub fn with_removal(mut self, marked: Removal, receiver_sites: Vec<String>) -> Self {
        match &mut self {
            Finding::MissingEndpoint { removal, .. } => *removal = Some(marked),
            Finding::OrphanedEndpoint { removal, sites, .. } => {
                *removal = Some(marked);
                *sites = receiver_sites;
            }
            _ => {}
        }
        self
    }

    /// The removal an endpoint row carries, when it lost a pairing.
    pub fn removal(&self) -> Option<&Removal> {
        match self {
            Finding::MissingEndpoint { removal, .. }
            | Finding::OrphanedEndpoint { removal, .. } => removal.as_ref(),
            _ => None,
        }
    }

    /// Attach producer-side provenance (real route vs mock/test handler) to a
    /// finding that has a producer side. Constructors default to `Route`, so
    /// existing call sites stay unchanged; the matchers chain this where the
    /// producer's classification is known. No-op for kinds without a
    /// producer-side provenance field.
    pub fn with_producer_provenance(mut self, producer: EndpointProvenance) -> Self {
        match &mut self {
            Finding::TypeMismatch {
                producer_provenance,
                ..
            } => *producer_provenance = producer,
            Finding::OrphanedEndpoint { provenance, .. } => *provenance = producer,
            _ => {}
        }
        self
    }

    /// State whether the rows behind this finding's edge are the source's or
    /// the model's (cloud#599). `None` leaves the field off the wire, which
    /// reads as "not stated". No-op for kinds that carry no edge.
    pub fn with_edge_source(mut self, source: Option<EdgeSource>) -> Self {
        match &mut self {
            Finding::TypeMismatch { edge_source, .. }
            | Finding::MethodMismatch { edge_source, .. } => *edge_source = source,
            _ => {}
        }
        self
    }

    /// State how far the type layer got on this finding's pair (cloud#599).
    /// No-op for kinds that carry no pair.
    pub fn with_verdict_state(mut self, state: Option<VerdictState>) -> Self {
        match &mut self {
            Finding::TypeMismatch { verdict_state, .. }
            | Finding::MethodMismatch { verdict_state, .. } => *verdict_state = state,
            _ => {}
        }
        self
    }

    /// State which half of the contract a type mismatch is about
    /// (carrick#1033), from the pair outcome's own `type_kind`. No-op for
    /// every other kind, and for a mismatch raised with no direction behind
    /// it the field stays `None`, which is what keeps a pair of side labels
    /// from reading as a pair of types.
    pub fn with_direction(mut self, kind: Option<crate::cloud_storage::ManifestTypeKind>) -> Self {
        if let Finding::TypeMismatch { direction, .. } = &mut self {
            *direction = kind;
        }
        self
    }

    /// State where a consumer with no type of its own reads what the
    /// producer's response does not provide (carrick#1517). A non-empty list
    /// clears `consumer_type`: that consumer's type is not known, and a
    /// generated alias would read as one. No-op for every other kind, and
    /// for an empty list.
    pub fn with_consumer_reads(mut self, reads: Vec<String>) -> Self {
        if reads.is_empty() {
            return self;
        }
        if let Finding::TypeMismatch {
            consumer_type,
            consumer_reads,
            ..
        } = &mut self
        {
            consumer_type.clear();
            *consumer_reads = reads;
        }
        self
    }

    /// State the pairing a mismatch is about (carrick-cloud#1369), the key a
    /// PR run's finding is compared with main's on. No-op for every other
    /// kind.
    pub fn with_pair(mut self, key: Option<String>) -> Self {
        match &mut self {
            Finding::TypeMismatch { pair, .. } | Finding::MethodMismatch { pair, .. } => {
                *pair = key
            }
            _ => {}
        }
        self
    }

    /// The pairing a mismatch is about, when its emitter stated one.
    pub fn pair(&self) -> Option<&str> {
        match self {
            Finding::TypeMismatch { pair, .. } | Finding::MethodMismatch { pair, .. } => {
                pair.as_deref()
            }
            _ => None,
        }
    }

    /// Whether main already had this finding (carrick-cloud#1369). Clears any
    /// reason the run could not tell. No-op for every kind but the mismatches
    /// and a dependency conflict (carrick#2283).
    pub fn set_on_main(&mut self, value: Option<bool>) {
        match self {
            Finding::TypeMismatch {
                on_main,
                on_main_unknown,
                ..
            }
            | Finding::MethodMismatch {
                on_main,
                on_main_unknown,
                ..
            }
            | Finding::DependencyConflict {
                on_main,
                on_main_unknown,
                ..
            } => {
                *on_main = value;
                *on_main_unknown = None;
            }
            _ => {}
        }
    }

    /// The run could not tell whether main already had this finding, and why
    /// (carrick-cloud#1408). No-op for every kind but the mismatches and a
    /// dependency conflict (carrick#2283).
    pub fn set_on_main_unknown(&mut self, reason: OnMainUnknown) {
        match self {
            Finding::TypeMismatch {
                on_main,
                on_main_unknown,
                ..
            }
            | Finding::MethodMismatch {
                on_main,
                on_main_unknown,
                ..
            }
            | Finding::DependencyConflict {
                on_main,
                on_main_unknown,
                ..
            } => {
                *on_main = None;
                *on_main_unknown = Some(reason);
            }
            _ => {}
        }
    }

    /// The one break answer for this finding (plan R2). See [`BreakStatus`].
    pub fn break_status(&self) -> BreakStatus {
        match self {
            Finding::TypeMismatch {
                edge_source,
                verdict_state,
                on_main,
                on_main_unknown,
                ..
            }
            | Finding::MethodMismatch {
                edge_source,
                verdict_state,
                on_main,
                on_main_unknown,
                ..
            } => mismatch_break_status(*edge_source, *verdict_state, *on_main, *on_main_unknown),
            // A row that lost a pairing main had, of any protocol and either
            // side (`missing_endpoint`, `orphaned_endpoint`). It is proven
            // only with a fact edge, a verdict that was not withheld and a
            // marking against main; anything less is a candidate.
            Finding::MissingEndpoint { removal, .. }
            | Finding::OrphanedEndpoint { removal, .. } => match removal {
                None => BreakStatus::NotABreak,
                Some(removal) => {
                    let marked = removal.on_main.is_some() || removal.on_main_unknown.is_some();
                    if removal.edge_source != Some(EdgeSource::Fact)
                        || removal.verdict_state == Some(VerdictState::Unresolved)
                        || !marked
                    {
                        BreakStatus::Candidate {
                            reason: "removal_not_proven",
                        }
                    } else {
                        mismatch_break_status(
                            removal.edge_source,
                            removal.verdict_state,
                            removal.on_main,
                            removal.on_main_unknown,
                        )
                    }
                }
            },
            Finding::EnvVarCall { .. }
            | Finding::DispatchOperations { .. }
            | Finding::DependencyConflict { .. }
            | Finding::SharedExternalContract { .. }
            | Finding::DegradedTypes { .. } => BreakStatus::NotABreak,
        }
    }

    /// Mark an orphaned endpoint as served by a module that also renders a
    /// view (carrick#704). No-op for every other kind.
    pub fn with_view_module(mut self, is_view_module: bool) -> Self {
        if let Finding::OrphanedEndpoint { view_module, .. } = &mut self {
            *view_module = is_view_module;
        }
        self
    }

    pub fn env_var_call(
        method: impl Into<String>,
        path: impl Into<String>,
        env_var: impl Into<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::EnvVarCall {
            method: method.into(),
            path: path.into(),
            env_var: env_var.into(),
            call_sites,
        }
    }

    pub fn dependency_conflict(
        package_name: impl Into<String>,
        tier: impl Into<String>,
        versions: Vec<PackageVersionRef>,
    ) -> Self {
        Finding::DependencyConflict {
            package_name: package_name.into(),
            tier: tier.into(),
            versions,
            changed_by_pr: Vec::new(),
            on_main: None,
            on_main_unknown: None,
        }
    }

    pub fn shared_external_contract(
        method: impl Into<String>,
        path: impl Into<String>,
        repos: Vec<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::SharedExternalContract {
            method: method.into(),
            path: path.into(),
            repos,
            call_sites,
        }
    }

    /// Type extraction failed for `service` at `stage`.
    pub fn degraded_types(
        service: impl Into<String>,
        stage: impl Into<String>,
        detail: &str,
    ) -> Self {
        Finding::DegradedTypes {
            service: service.into(),
            stage: stage.into(),
            detail: truncate_chars(detail, MAX_DETAIL_CHARS),
        }
    }

    /// The wire `kind` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::TypeMismatch { .. } => "type_mismatch",
            Finding::MethodMismatch { .. } => "method_mismatch",
            Finding::MissingEndpoint { .. } => "missing_endpoint",
            Finding::OrphanedEndpoint { .. } => "orphaned_endpoint",
            Finding::EnvVarCall { .. } => "env_var_call",
            Finding::DispatchOperations { .. } => "dispatch_operations",
            Finding::DependencyConflict { .. } => "dependency_conflict",
            Finding::SharedExternalContract { .. } => "shared_external_contract",
            Finding::DegradedTypes { .. } => "degraded_types",
        }
    }

    /// Severity is a function of the kind (plus tier for dependency
    /// conflicts), per the wire contract's table — never stored, so it can't
    /// drift from the kind.
    pub fn severity(&self) -> Severity {
        match self {
            Finding::TypeMismatch { .. }
            | Finding::MethodMismatch { .. }
            | Finding::DegradedTypes { .. } => Severity::Risk,
            // A row that lost a pairing main had is a break (carrick#3092).
            Finding::MissingEndpoint { removal, .. }
            | Finding::OrphanedEndpoint { removal, .. } => {
                if removal.is_some() {
                    Severity::Risk
                } else {
                    Severity::Gap
                }
            }
            Finding::EnvVarCall { .. }
            | Finding::SharedExternalContract { .. }
            | Finding::DispatchOperations { .. } => Severity::Advisory,
            Finding::DependencyConflict { tier, .. } => {
                if tier == tier::MAJOR {
                    Severity::Gap
                } else {
                    Severity::Advisory
                }
            }
        }
    }
}

/// Hand-rolled so `severity` is emitted alongside the `kind` tag without
/// being a stored field (a derived internally-tagged enum can't add a
/// computed sibling field).
impl Serialize for Finding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", self.kind())?;
        map.serialize_entry("severity", &self.severity())?;
        // The one break answer (plan R2, B1), written on every kind so a
        // reader never folds `edge_source`, `verdict_state` and `on_main`
        // itself. Computed, not stored, like `severity`.
        let status = self.break_status();
        map.serialize_entry("break", status.wire_label())?;
        if let Some(reason) = status.reason() {
            map.serialize_entry("break_reason", reason)?;
        }
        match self {
            Finding::TypeMismatch {
                method,
                path,
                service,
                call_sites,
                producer_type,
                consumer_type,
                detail,
                producer_provenance,
                edge_source,
                verdict_state,
                // Scan-local (carrick#1033): the local join reads it off the
                // in-process finding, and this payload's shape is the cloud's
                // as well, so it is not written here.
                direction: _,
                // Scan-local (carrick-cloud#1369): the comparison key, not a
                // fact a reader needs.
                pair: _,
                // Scan-local (carrick#1517): `detail` already names each read.
                consumer_reads: _,
                on_main,
                on_main_unknown,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
                map.serialize_entry("producer_type", producer_type)?;
                map.serialize_entry("consumer_type", consumer_type)?;
                map.serialize_entry("detail", detail)?;
                map.serialize_entry("producer_provenance", producer_provenance)?;
                // Both omitted when unstated, so a scan that cannot say leaves
                // the payload exactly as it was before these fields existed.
                if let Some(edge_source) = edge_source {
                    map.serialize_entry("edge_source", edge_source)?;
                }
                if let Some(verdict_state) = verdict_state {
                    map.serialize_entry("verdict_state", verdict_state)?;
                }
                serialize_on_main(&mut map, on_main, on_main_unknown)?;
            }
            Finding::MethodMismatch {
                method,
                path,
                service,
                call_sites,
                expected_method,
                edge_source,
                verdict_state,
                pair: _,
                on_main,
                on_main_unknown,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
                map.serialize_entry("expected_method", expected_method)?;
                if let Some(edge_source) = edge_source {
                    map.serialize_entry("edge_source", edge_source)?;
                }
                if let Some(verdict_state) = verdict_state {
                    map.serialize_entry("verdict_state", verdict_state)?;
                }
                serialize_on_main(&mut map, on_main, on_main_unknown)?;
            }
            Finding::MissingEndpoint {
                method,
                path,
                service,
                call_sites,
                removal,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
                if let Some(removal) = removal {
                    serialize_removal(&mut map, removal)?;
                }
            }
            Finding::OrphanedEndpoint {
                method,
                path,
                service,
                provenance,
                view_module,
                sites,
                removal,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("provenance", provenance)?;
                // Skipped when false, so the common row is unchanged.
                if *view_module {
                    map.serialize_entry("view_module", view_module)?;
                }
                if let Some(removal) = removal {
                    map.serialize_entry("sites", wire_call_sites(sites))?;
                    serialize_removal(&mut map, removal)?;
                }
            }
            Finding::EnvVarCall {
                method,
                path,
                env_var,
                call_sites,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("env_var", env_var)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::DispatchOperations {
                service,
                route,
                dispatch_location,
                dispatch_field,
                values,
                call_sites,
            } => {
                map.serialize_entry("service", service)?;
                // Always written, null included: a reader has to tell "no
                // route in the source" from "a field the writer forgot".
                map.serialize_entry("route", route)?;
                map.serialize_entry("dispatch_location", dispatch_location)?;
                map.serialize_entry("dispatch_field", dispatch_field)?;
                map.serialize_entry("values", values)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::DependencyConflict {
                package_name,
                tier,
                versions,
                changed_by_pr,
                on_main,
                on_main_unknown,
            } => {
                map.serialize_entry("package_name", package_name)?;
                map.serialize_entry("tier", tier)?;
                map.serialize_entry("versions", versions)?;
                // Omitted when the PR changed nothing, and on a run that is
                // not a PR run.
                if !changed_by_pr.is_empty() {
                    map.serialize_entry("changed_by_pr", changed_by_pr)?;
                }
                serialize_on_main(&mut map, on_main, on_main_unknown)?;
            }
            Finding::SharedExternalContract {
                method,
                path,
                repos,
                call_sites,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("repos", repos)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::DegradedTypes {
                service,
                stage,
                detail,
            } => {
                map.serialize_entry("service", service)?;
                map.serialize_entry("stage", stage)?;
                map.serialize_entry("detail", detail)?;
            }
        }
        map.end()
    }
}

/// An endpoint reference in the PR delta. `service` is emitted as explicit
/// `null` when unattributed (the wire shows it, unlike `verified` entries).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EndpointRef {
    pub method: String,
    pub path: String,
    pub service: Option<String>,
    /// Whether the operation's rows are facts or candidates, folded over the
    /// `resolution_source` of every row of it by [`EdgeSource::fold`] (the
    /// split is [`ResolutionSource::is_candidate`]): `fact` when a
    /// deterministic pass (the trace, a library claim, a descriptor, a schema)
    /// stated every row, `candidate` when the model stated any. A new
    /// endpoint's rows are this run's; a removed endpoint's are main's stored
    /// index's. Omitted when a row states no source, as on an index written
    /// before the field existed. A removed HTTP route is `candidate` whatever
    /// its rows were when the PR wrote a route site this run could not read a
    /// path for that could serve it (carrick#3222; `docs/reference/runner.md`,
    /// "The endpoint delta").
    ///
    /// [`ResolutionSource::is_candidate`]: crate::agents::file_analyzer_agent::ResolutionSource::is_candidate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_source: Option<EdgeSource>,
}

/// What this PR changed relative to the repo's last-indexed (main) state.
/// `None` at the payload level outside a PR diff baseline.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PrDelta {
    pub new_endpoints: Vec<EndpointRef>,
    pub removed_endpoints: Vec<EndpointRef>,
    /// The files whose operation changes this delta cannot list: a file the
    /// PR added or edited that this scan could not read, or one where main's
    /// index holds operations this scan does not state on lines the PR did
    /// not change (carrick#3233). Sorted by (service, file). Absent from the
    /// wire when empty, so a payload with nothing unread is unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files_not_read: Vec<FileNotRead>,
}

/// Why the delta cannot list a file's operation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileNotReadReason {
    /// The file does not parse, so no row of it is stated.
    DoesNotParse,
    /// The file is not UTF-8 text, so it was never read.
    NotUtf8Text,
    /// The file was read, and main's index holds operations in it that this
    /// scan does not state, on lines the PR did not change: they are not
    /// listed as removed, and other changes in that file may be missing.
    OperationsMissing,
}

impl FileNotReadReason {
    /// The reason as the wire spells it.
    pub fn code(self) -> &'static str {
        match self {
            FileNotReadReason::DoesNotParse => "does_not_parse",
            FileNotReadReason::NotUtf8Text => "not_utf8_text",
            FileNotReadReason::OperationsMissing => "operations_missing",
        }
    }

    /// The reason in words, for a line that says why a file was not read;
    /// `None` when the reason is never shown. These are the PR check's own
    /// phrases (carrick-cloud `lambdas/_shared/render_pr_comment.ts`,
    /// `UNREAD_REASON_PHRASE`): keep them equal.
    pub fn phrase(self) -> Option<&'static str> {
        match self {
            FileNotReadReason::DoesNotParse => Some("it does not parse"),
            FileNotReadReason::NotUtf8Text => Some("it is not UTF-8 text"),
            FileNotReadReason::OperationsMissing => None,
        }
    }
}

/// One file of [`PrDelta::files_not_read`]. A file both unread and holding
/// main's lost operations takes the read failure as its reason.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FileNotRead {
    /// Relative to the scan root, as a row's `file_path` names it.
    pub file: String,
    /// `null` when unattributed, as on [`EndpointRef`].
    pub service: Option<String>,
    pub reason: FileNotReadReason,
    /// Main's operations in the file that this scan does not state, every
    /// protocol, sorted as the delta's rows are. Empty when main held none.
    pub operations: Vec<EndpointRef>,
}

impl PrDelta {
    pub fn is_empty(&self) -> bool {
        self.new_endpoints.is_empty() && self.removed_endpoints.is_empty()
    }
}

/// Shape of the project being analyzed: a lone repo, a monorepo (multiple
/// services declared in one carrick.json), or a poly-repo project (peer repos
/// indexed alongside this one). The formatter frames findings with it; the
/// cloud applies the same baseline gating when rendering the PR comment.
#[derive(Clone, Debug, Serialize)]
pub struct Topology {
    /// This repo's name, used to title single-repo comments.
    pub repo_name: String,
    /// Services declared for THIS repo. More than one means a monorepo.
    pub local_service_count: usize,
    /// Other repos indexed for the project (peers).
    pub peer_repo_count: usize,
}

/// Index size for the scanned project.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ScanStats {
    pub endpoints: usize,
    pub calls: usize,
}

/// An endpoint at least one consumer call successfully matched.
#[derive(Clone, Debug, Serialize)]
pub struct VerifiedEndpoint {
    pub method: String,
    pub path: String,
    /// Real route vs mock/test handler (#380); route-wins when several
    /// producers share the (method, path) key.
    pub provenance: EndpointProvenance,
    /// Aggregated per-endpoint type verdict (#455). Drives the cloud
    /// PR-comment renderer's honest "Verified" buckets: `Some(Compatible)` →
    /// "Type-checked", `Some(Unverifiable)` → "Types not verifiable", anything
    /// else (absent, or `Incompatible` — already a loud finding above) →
    /// "Types not compared". Omitted from the wire when absent, so an older
    /// payload and a not-type-checked pair both read as "not compared".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_verdict: Option<crate::operation::TypeVerdict>,
    /// carrick#1516: `type_verdict` is `Compatible` and at least one pair
    /// agreed only on what the producer's handler returns, its published type
    /// being wider. Off the wire when false, so every other row is what the
    /// pre-#1516 scanner sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub producer_wider: bool,
}

/// GraphQL coverage signal: libraries detected vs operations actually
/// indexed. Drives the "commit an emitted schema" banner.
#[derive(Clone, Debug, Serialize)]
pub struct GraphqlStatus {
    pub libraries: Vec<String>,
    pub operations_indexed: bool,
}

/// The `post-pr-result` payload. The transport layer (aws_storage) adds the
/// `action` and `schema_version` envelope fields; everything else on the wire
/// is exactly this struct.
#[derive(Clone, Debug, Serialize)]
pub struct PrResultPayload {
    /// Self-reported repo name — the cloud ignores it (identity comes from
    /// the OIDC token) but it aids log correlation.
    pub repo: String,
    pub pr_number: u64,
    /// `pull_request.head.sha` from GITHUB_EVENT_PATH; `null` if unavailable.
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub topology: Topology,
    pub stats: ScanStats,
    pub findings: Vec<Finding>,
    pub delta: Option<PrDelta>,
    pub verified: Vec<VerifiedEndpoint>,
    pub graphql: GraphqlStatus,
    /// Whether every scanned service resolved its types this run. `false`
    /// when any service degraded (see the `degraded_types` findings), so a
    /// surface reading the payload can say "types were not read" instead of
    /// inferring it from an absent type verdict.
    pub has_types: bool,
    /// Which index the run compared with (carrick#2284). `None` on a run
    /// that is not a PR run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
    /// What the run checked (carrick#2284). `None` on a run that is not a PR
    /// run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<Checked>,
    /// The functions the PR added, edited or removed (carrick#2285). `None`
    /// on a run that is not a PR run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions: Option<FunctionsBlock>,
}

/// The functions a pull request added, edited or removed, named by comparing
/// each changed file with the copy its base commit holds (carrick#2285), or
/// why the run could not name them.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FunctionsBlock {
    Named(NamedFunctions),
    Unknown { unknown: FunctionsUnknown },
}

/// Why a PR run could not name the functions the PR changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionsUnknown {
    /// The run does not know the commit the PR's tree is based on.
    NoBaseCommit,
    /// Git could not read the base commit or a file in it: a clone that does
    /// not hold it.
    BaseUnavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NamedFunctions {
    /// At most [`MAX_CHANGED_FUNCTIONS`]: added before edited, exported
    /// first, then by file and line.
    pub changed: Vec<ChangedFunction>,
    /// At most [`MAX_REMOVED_FUNCTIONS`], by file and name.
    pub removed: Vec<FunctionRef>,
    pub counts: FunctionCounts,
}

/// Most entries `functions.changed` carries.
pub const MAX_CHANGED_FUNCTIONS: usize = 100;
/// Most entries `functions.removed` carries.
pub const MAX_REMOVED_FUNCTIONS: usize = 200;
/// Most entries one function's `calls` or `called_by` carries.
pub const MAX_FUNCTION_EDGES: usize = 50;

/// A function by where the index holds it: the path the uploaded blob
/// carries for its file, and its name as the index spells it
/// (`Class.member` for a method).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FunctionRef {
    pub file: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionChange {
    Added,
    Edited,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChangedFunction {
    pub file: String,
    pub name: String,
    pub line: u32,
    pub end_line: u32,
    pub change: FunctionChange,
    pub exported: bool,
    /// This run's description of the function, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The file's path at the base commit, when git reports it renamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    /// The functions it calls, as far as the scan resolves them.
    pub calls: Vec<FunctionRef>,
    /// The functions in this repo's services that call it.
    pub called_by: Vec<FunctionRef>,
}

/// How many functions the PR changed. `added`, `edited` and `removed` count
/// every one, listed or not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FunctionCounts {
    pub added: usize,
    pub edited: usize,
    pub removed: usize,
    /// Listed entries of `changed` that carry no `intent`.
    pub without_intent: usize,
    /// Entries left out of `changed` by its cap.
    pub omitted: usize,
}

/// The index a PR run compared with: main's copy of this repo and every
/// peer's, by commit (carrick#2284). Two posts for one pull request can
/// differ because either moved, and this is what shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Baseline {
    /// The commit the PR's tree is based on; `null` when the run does not
    /// know it.
    pub base_sha: Option<String>,
    pub copy: BaselineCopy,
    /// One entry per stored service of this repo, sorted by service.
    pub main_index: Vec<MainIndexEntry>,
    pub peers: PeerIndexes,
}

/// Whether main's copy of this repo is main as the PR's base has it
/// (`pr_baseline::MainCopy`), or absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineCopy {
    Current,
    Stale,
    OtherScanner,
    /// This repo has no index on main.
    None,
}

/// One stored service of this repo in main's copy.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MainIndexEntry {
    /// `null` for an unnamed service.
    pub service: Option<String>,
    pub commit: String,
    /// Omitted when the copy states none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanner_version: Option<String>,
    /// Written only when the copy was scanned from a tree with uncommitted
    /// changes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}

/// The peer indexes a run read, as a count and one value that moves when any
/// of them does.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PeerIndexes {
    pub count: usize,
    /// Lowercase hex sha256 over the lines `repo|service|commit`, one per
    /// peer index, sorted and newline-terminated. An unnamed service is the
    /// empty string.
    pub sha256: String,
}

/// What a PR run checked (carrick#2284).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Checked {
    /// Direct producer-consumer pairs. Each one outside `not_compared` is one
    /// type comparison this run made.
    pub pairs: PairCounts,
    /// SDK-mediated edges, whose verdict is carried from the match the
    /// package's own call formed and is no comparison of its own. Omitted
    /// when the run has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_pairs: Option<PairCounts>,
    pub dependencies: DependencyCounts,
}

/// Operation pairs (HTTP, GraphQL, socket, pub/sub) with a producer or a
/// consumer in one of this repo's services, by type verdict. A pair is one
/// call site against one producer operation. The first three count pairs the
/// type check reached and say what it answered; `not_compared` counts the
/// pairs it did not reach, whatever the protocol. `compatible` holds only
/// pairs compared over two known types: a compatible verdict over an `any`
/// or `unknown` counts as `unverifiable`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PairCounts {
    pub compatible: usize,
    pub unverifiable: usize,
    pub incompatible: usize,
    pub not_compared: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DependencyCounts {
    /// Packages one of this repo's services and at least one other service
    /// both declare.
    pub shared: usize,
    /// Packages whose written specifier the PR changed in a service of this
    /// repo. Omitted when the run cannot say: this repo has no index on main,
    /// or main's copy is not main as the PR's base has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        // Multi-byte input: 'é' (2 bytes), '☕' (3 bytes). The old byte-sliced
        // truncation panicked on exactly this shape.
        let s = "café☕".repeat(100); // 500 chars, 900 bytes
        let out = truncate_chars(&s, 400);
        assert_eq!(out.chars().count(), 400);
        assert!(out.ends_with("..."));
        // Truncation point is a char boundary: the kept prefix is intact.
        assert!(s.starts_with(out.trim_end_matches("...")));

        // At or under the cap: returned unchanged, no ellipsis.
        assert_eq!(truncate_chars("café☕", 5), "café☕");
        assert_eq!(truncate_chars("café☕", 400), "café☕");

        // ASCII over the cap.
        assert_eq!(truncate_chars("abcdefgh", 6), "abc...");
    }

    #[test]
    fn truncate_chars_never_exceeds_max_even_for_tiny_max() {
        // max <= 3 leaves no room for the ellipsis: hard-cut instead. The
        // ≤ max contract must hold for every input (pub fn, release builds).
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abcdef", 2), "ab");
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(truncate_chars("café☕x", 2), "ca");
        for max in 0..6 {
            assert!(truncate_chars("abcdefgh", max).chars().count() <= max);
        }
        // Short input with tiny max: unchanged.
        assert_eq!(truncate_chars("ab", 2), "ab");
    }

    #[test]
    fn call_sites_capped_on_the_wire_only() {
        // The in-memory finding keeps every call site (the terminal report
        // shows true counts); only the serialized wire form is capped.
        let sites: Vec<String> = (0..9).map(|i| format!("src/a.ts:{i}")).collect();
        let finding = Finding::missing_endpoint("GET", "/x", None, sites);
        let Finding::MissingEndpoint { call_sites, .. } = &finding else {
            panic!("wrong variant");
        };
        assert_eq!(call_sites.len(), 9);

        let v = serde_json::to_value(&finding).unwrap();
        let wire_sites = v["call_sites"].as_array().unwrap();
        assert_eq!(wire_sites.len(), MAX_CALL_SITES);
        // The first five in order, not an arbitrary subset.
        assert_eq!(wire_sites[0], "src/a.ts:0");
        assert_eq!(wire_sites[4], "src/a.ts:4");
    }

    #[test]
    fn producer_provenance_defaults_to_route_and_serializes() {
        // Constructors default to `route`; the wire always carries the field
        // so the cloud renderer never has to infer it (#380).
        let mismatch = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", "boom");
        let v = serde_json::to_value(&mismatch).unwrap();
        assert_eq!(v["producer_provenance"], json!("route"));

        let mismatch = mismatch.with_producer_provenance(EndpointProvenance::Mock);
        let v = serde_json::to_value(&mismatch).unwrap();
        assert_eq!(v["producer_provenance"], json!("mock"));

        let orphan = Finding::orphaned_endpoint("GET", "/x", None)
            .with_producer_provenance(EndpointProvenance::Mock);
        let v = serde_json::to_value(&orphan).unwrap();
        assert_eq!(v["provenance"], json!("mock"));

        // No-op on kinds without a producer-side field.
        let missing = Finding::missing_endpoint("GET", "/x", None, vec![])
            .with_producer_provenance(EndpointProvenance::Mock);
        assert_eq!(
            missing,
            Finding::missing_endpoint("GET", "/x", None, vec![])
        );
    }

    /// carrick-cloud#1369, the wire half. `on_main` is written on both
    /// mismatch kinds when the PR run compared with main, as a boolean the
    /// renderer reads with `=== true`. The pairing it was compared on never
    /// reaches the wire.
    #[test]
    fn on_main_is_on_the_wire_and_the_pairing_is_not() {
        let mut mismatch = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", "boom")
            .with_pair(Some("api|GET|/x~web|src/a.ts|response".to_string()));
        mismatch.set_on_main(Some(true));
        let v = serde_json::to_value(&mismatch).unwrap();
        assert_eq!(v["on_main"], json!(true));
        assert!(!v.as_object().unwrap().contains_key("pair"));

        let mut wrong_verb = Finding::method_mismatch("PUT", "/x", None, vec![], "POST")
            .with_pair(Some("POST /x~PUT".to_string()));
        wrong_verb.set_on_main(Some(false));
        let v = serde_json::to_value(&wrong_verb).unwrap();
        assert_eq!(v["on_main"], json!(false));
        assert!(!v.as_object().unwrap().contains_key("pair"));
    }

    /// On a run that marks nothing (any run that is not a PR run) neither
    /// field is written, and the finding serializes exactly as it did before
    /// either existed, pairing or not.
    #[test]
    fn without_a_baseline_the_finding_is_unchanged_on_the_wire() {
        let before = serde_json::to_value(Finding::type_mismatch(
            "GET",
            "/x",
            None,
            vec!["src/a.ts:3".into()],
            "A",
            "B",
            "boom",
        ))
        .unwrap();
        let keyed = Finding::type_mismatch(
            "GET",
            "/x",
            None,
            vec!["src/a.ts:3".into()],
            "A",
            "B",
            "boom",
        )
        .with_pair(Some("api|GET|/x~web|src/a.ts|response".to_string()));
        assert_eq!(serde_json::to_value(&keyed).unwrap(), before);
        assert!(!before.as_object().unwrap().contains_key("on_main"));

        let before =
            serde_json::to_value(Finding::method_mismatch("PUT", "/x", None, vec![], "POST"))
                .unwrap();
        let keyed = Finding::method_mismatch("PUT", "/x", None, vec![], "POST")
            .with_pair(Some("POST /x~PUT".to_string()));
        assert_eq!(serde_json::to_value(&keyed).unwrap(), before);
        assert!(!before.as_object().unwrap().contains_key("on_main"));
        assert!(!before.as_object().unwrap().contains_key("on_main_unknown"));
    }

    /// carrick-cloud#1408, the wire half. A PR run that cannot tell whether
    /// main had a finding sends why, in snake_case, in place of `on_main`; the
    /// two are never sent together, and a later verdict replaces the reason.
    #[test]
    fn a_finding_the_run_could_not_compare_says_why_on_the_wire() {
        use OnMainUnknown::*;
        for (reason, spelled) in [
            (NoMainIndex, "no_main_index"),
            (MainIndexStale, "main_index_stale"),
            (MainIndexOtherScanner, "main_index_other_scanner"),
            (MainSideFailed, "main_side_failed"),
            (MainTypesUnjudged, "main_types_unjudged"),
            (RemovalUnplaced, "removal_unplaced"),
        ] {
            let mut mismatch = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", "boom")
                .with_pair(Some("api|GET|/x~web|src/a.ts|response".to_string()));
            mismatch.set_on_main_unknown(reason);
            let v = serde_json::to_value(&mismatch).unwrap();
            assert_eq!(v["on_main_unknown"], json!(spelled));
            assert!(!v.as_object().unwrap().contains_key("on_main"));

            let mut wrong_verb = Finding::method_mismatch("PUT", "/x", None, vec![], "POST")
                .with_pair(Some("POST /x~PUT".to_string()));
            wrong_verb.set_on_main_unknown(reason);
            let v = serde_json::to_value(&wrong_verb).unwrap();
            assert_eq!(v["on_main_unknown"], json!(spelled));

            wrong_verb.set_on_main(Some(true));
            let v = serde_json::to_value(&wrong_verb).unwrap();
            assert_eq!(v["on_main"], json!(true));
            assert!(!v.as_object().unwrap().contains_key("on_main_unknown"));
        }
    }

    #[test]
    fn detail_truncated_at_construction() {
        let long = "☕".repeat(500);
        let finding = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", &long);
        let Finding::TypeMismatch { detail, .. } = &finding else {
            panic!("wrong variant");
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS);
        assert!(detail.ends_with("..."));
    }

    /// A dead sidecar costs the run every type verdict, so the loss has to
    /// reach the comment as a finding the renderer can show. The cloud
    /// buckets an unrecognised kind by its self-reported `severity` and
    /// renders `detail`, so a scanner ahead of the cloud still surfaces it
    /// (carrick#535).
    #[test]
    fn degraded_types_serializes_as_a_risk_with_detail() {
        let finding =
            Finding::degraded_types("api", "capture", "Sidecar process died unexpectedly");
        let v = serde_json::to_value(&finding).unwrap();
        assert_eq!(v["kind"], json!("degraded_types"));
        assert_eq!(v["severity"], json!("risk"));
        assert_eq!(v["service"], json!("api"));
        assert_eq!(v["stage"], json!("capture"));
        assert_eq!(v["detail"], json!("Sidecar process died unexpectedly"));

        let long = "e".repeat(900);
        let Finding::DegradedTypes { detail, .. } =
            Finding::degraded_types("api", "resolve", &long)
        else {
            panic!("wrong variant");
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS);
    }

    /// Snapshot of the wire shape: kind tags, severity strings, tier values,
    /// and field names must match docs/internal/pr-result-pipeline.md
    /// (schema_version 1) in carrick-cloud exactly.
    #[test]
    fn payload_serializes_to_wire_contract_shape() {
        let payload = PrResultPayload {
            repo: "api-server".to_string(),
            pr_number: 123,
            head_sha: None,
            run_id: Some("9876543210".to_string()),
            topology: Topology {
                repo_name: "api-server".to_string(),
                local_service_count: 1,
                peer_repo_count: 2,
            },
            stats: ScanStats {
                endpoints: 42,
                calls: 17,
            },
            findings: vec![
                Finding::type_mismatch(
                    "GET",
                    "/api/users",
                    None,
                    vec!["web/src/client.ts:12".to_string()],
                    "UserResponse",
                    "User[]",
                    "Property 'role' is missing",
                ),
                Finding::method_mismatch(
                    "GET",
                    "/api/orders",
                    None,
                    vec!["web/src/orders.ts:4".to_string()],
                    "POST",
                ),
                Finding::missing_endpoint(
                    "DELETE",
                    "/api/sessions",
                    None,
                    vec!["web/src/auth.ts:9".to_string()],
                ),
                Finding::orphaned_endpoint("GET", "/legacy/ping", Some("billing".to_string())),
                Finding::env_var_call(
                    "GET",
                    "/orders",
                    "ORDER_SERVICE_URL",
                    vec!["src/orders.ts:3".to_string()],
                ),
                Finding::dependency_conflict(
                    "zod",
                    tier::MAJOR,
                    vec![PackageVersionRef {
                        repo: "billing".to_string(),
                        version: "4.0.0".to_string(),
                        source: "package.json".to_string(),
                        spec: String::new(),
                    }],
                ),
                Finding::dependency_conflict("typescript", tier::UNPARSEABLE, vec![]),
                Finding::shared_external_contract(
                    "POST",
                    "/v2/widgets",
                    vec!["repo-alpha".to_string(), "repo-beta".to_string()],
                    vec!["src/widgets-client.ts:33".to_string()],
                ),
            ],
            delta: Some(PrDelta {
                new_endpoints: vec![EndpointRef {
                    method: "GET".to_string(),
                    path: "/x".to_string(),
                    service: None,
                    edge_source: None,
                }],
                removed_endpoints: vec![EndpointRef {
                    method: "GET".to_string(),
                    path: "/y".to_string(),
                    service: None,
                    edge_source: None,
                }],
                files_not_read: Vec::new(),
            }),
            verified: vec![VerifiedEndpoint {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: Default::default(),
                type_verdict: None,
                producer_wider: false,
            }],
            graphql: GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
            baseline: None,
            checked: None,
            functions: None,
        };

        let v = serde_json::to_value(&payload).unwrap();

        assert_eq!(v["repo"], "api-server");
        assert_eq!(v["pr_number"], 123);
        assert_eq!(v["head_sha"], serde_json::Value::Null);
        assert_eq!(v["run_id"], "9876543210");
        assert_eq!(
            v["topology"],
            json!({ "repo_name": "api-server", "local_service_count": 1, "peer_repo_count": 2 })
        );
        assert_eq!(v["stats"], json!({ "endpoints": 42, "calls": 17 }));
        assert_eq!(
            v["delta"],
            json!({
                "new_endpoints": [{ "method": "GET", "path": "/x", "service": null }],
                "removed_endpoints": [{ "method": "GET", "path": "/y", "service": null }],
            })
        );
        assert_eq!(
            v["verified"],
            json!([{ "method": "GET", "path": "/api/users", "provenance": "route" }])
        );
        assert_eq!(
            v["graphql"],
            json!({ "libraries": [], "operations_indexed": false })
        );

        let findings = v["findings"].as_array().unwrap();
        assert_eq!(
            findings[0],
            json!({
                "kind": "type_mismatch",
                "severity": "risk",
                "break": "possible",
                "break_reason": "not_compared_with_main",
                "method": "GET",
                "path": "/api/users",
                "service": null,
                "call_sites": ["web/src/client.ts:12"],
                "producer_type": "UserResponse",
                "consumer_type": "User[]",
                "detail": "Property 'role' is missing",
                "producer_provenance": "route",
            })
        );
        assert_eq!(
            findings[1],
            json!({
                "kind": "method_mismatch",
                "severity": "risk",
                "break": "possible",
                "break_reason": "not_compared_with_main",
                "method": "GET",
                "path": "/api/orders",
                "service": null,
                "call_sites": ["web/src/orders.ts:4"],
                "expected_method": "POST",
            })
        );
        assert_eq!(
            findings[2],
            json!({
                "kind": "missing_endpoint",
                "severity": "gap",
                "break": "none",
                "method": "DELETE",
                "path": "/api/sessions",
                "service": null,
                "call_sites": ["web/src/auth.ts:9"],
            })
        );
        assert_eq!(
            findings[3],
            json!({
                "kind": "orphaned_endpoint",
                "severity": "gap",
                "break": "none",
                "method": "GET",
                "path": "/legacy/ping",
                "service": "billing",
                "provenance": "route",
            })
        );
        assert_eq!(
            findings[4],
            json!({
                "kind": "env_var_call",
                "severity": "advisory",
                "break": "none",
                "method": "GET",
                "path": "/orders",
                "env_var": "ORDER_SERVICE_URL",
                "call_sites": ["src/orders.ts:3"],
            })
        );
        assert_eq!(
            findings[5],
            json!({
                "kind": "dependency_conflict",
                "severity": "gap",
                "break": "none",
                "package_name": "zod",
                "tier": "major",
                "versions": [{ "repo": "billing", "version": "4.0.0", "source": "package.json" }],
            })
        );
        // Unparseable tier downgrades the severity to advisory.
        assert_eq!(findings[6]["severity"], "advisory");
        assert_eq!(findings[6]["tier"], "unparseable");
        // Shared external contract (#379): role-free — repos, never a
        // producer/consumer or service attribution.
        assert_eq!(
            findings[7],
            json!({
                "kind": "shared_external_contract",
                "severity": "advisory",
                "break": "none",
                "method": "POST",
                "path": "/v2/widgets",
                "repos": ["repo-alpha", "repo-beta"],
                "call_sites": ["src/widgets-client.ts:33"],
            })
        );
    }

    fn zod_conflict(spec: &str) -> Finding {
        Finding::dependency_conflict(
            "zod",
            tier::MAJOR,
            vec![PackageVersionRef {
                repo: "api".to_string(),
                version: "4.0.0".to_string(),
                source: "services/api/package.json".to_string(),
                spec: spec.to_string(),
            }],
        )
    }

    /// carrick#2283: a dependency conflict nobody marked, from a copy with no
    /// written specifier, is byte for byte what it was before the PR fields
    /// existed. Each field appears only when it has something to say, and
    /// `on_main` never beside `on_main_unknown`.
    #[test]
    fn a_dependency_conflict_writes_its_pr_fields_only_when_set() {
        let bytes = |finding: &Finding| serde_json::to_string(finding).unwrap();
        const BARE: &str = r#"{"kind":"dependency_conflict","severity":"gap","break":"none","package_name":"zod","tier":"major","versions":[{"repo":"api","version":"4.0.0","source":"services/api/package.json"}]}"#;
        assert_eq!(bytes(&zod_conflict("")), BARE);

        const WRITTEN: &str = r#"{"kind":"dependency_conflict","severity":"gap","break":"none","package_name":"zod","tier":"major","versions":[{"repo":"api","version":"4.0.0","source":"services/api/package.json","spec":"^4.0.0"}]"#;
        let mut marked = zod_conflict("^4.0.0");
        assert_eq!(bytes(&marked), format!("{WRITTEN}}}"));

        marked.set_on_main(Some(false));
        assert_eq!(bytes(&marked), format!(r#"{WRITTEN},"on_main":false}}"#));

        let Finding::DependencyConflict { changed_by_pr, .. } = &mut marked else {
            unreachable!()
        };
        changed_by_pr.push(SpecChange {
            service: "api".to_string(),
            from: Some("^3.22.0".to_string()),
            to: None,
        });
        assert_eq!(
            bytes(&marked),
            format!(
                r#"{WRITTEN},"changed_by_pr":[{{"service":"api","from":"^3.22.0","to":null}}],"on_main":false}}"#
            )
        );

        marked.set_on_main_unknown(OnMainUnknown::MainIndexStale);
        let value = serde_json::to_value(&marked).unwrap();
        assert_eq!(value["on_main_unknown"], json!("main_index_stale"));
        assert!(value.get("on_main").is_none(), "{value:#}");
        marked.set_on_main(Some(true));
        let value = serde_json::to_value(&marked).unwrap();
        assert_eq!(value["on_main"], json!(true));
        assert!(value.get("on_main_unknown").is_none(), "{value:#}");
    }

    /// #455: a verified entry's `type_verdict` serializes to the exact
    /// lowercase strings the cloud PR-comment renderer keys its honest buckets
    /// on (`"compatible"` / `"unverifiable"` / `"incompatible"`), and is
    /// OMITTED entirely when absent so an older payload and a not-type-checked
    /// pair both read as "Types not compared".
    #[test]
    fn verified_endpoint_type_verdict_wire_strings() {
        use crate::operation::TypeVerdict;

        let with = |v: Option<TypeVerdict>| {
            serde_json::to_value(VerifiedEndpoint {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: Default::default(),
                type_verdict: v,
                producer_wider: false,
            })
            .unwrap()
        };

        assert_eq!(
            with(Some(TypeVerdict::Compatible))["type_verdict"],
            "compatible"
        );
        assert_eq!(
            with(Some(TypeVerdict::Unverifiable))["type_verdict"],
            "unverifiable"
        );
        assert_eq!(
            with(Some(TypeVerdict::Incompatible))["type_verdict"],
            "incompatible"
        );
        // Absent verdict: the key must not appear at all.
        let none = with(None);
        assert!(
            none.get("type_verdict").is_none(),
            "an absent verdict must be omitted from the wire, got: {none}"
        );
    }

    /// carrick#1516: the producer-wider class rides beside a `compatible`
    /// verdict, and a row without it is exactly the pre-#1516 row.
    #[test]
    fn verified_endpoint_producer_wider_rides_beside_the_verdict() {
        let with = |producer_wider: bool| {
            serde_json::to_string(&VerifiedEndpoint {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: Default::default(),
                type_verdict: Some(crate::operation::TypeVerdict::Compatible),
                producer_wider,
            })
            .unwrap()
        };
        assert_eq!(
            with(false),
            r#"{"method":"GET","path":"/api/users","provenance":"route","type_verdict":"compatible"}"#
        );
        assert_eq!(
            with(true),
            r#"{"method":"GET","path":"/api/users","provenance":"route","type_verdict":"compatible","producer_wider":true}"#
        );
    }

    /// An empty `run_id` is omitted, not sent as `""` — the cloud validates
    /// it against `/^\d+$/` and would drop the whole field anyway.
    #[test]
    fn run_id_omitted_when_absent() {
        let payload = PrResultPayload {
            repo: "r".to_string(),
            pr_number: 1,
            head_sha: Some("a".repeat(40)),
            run_id: None,
            topology: Topology {
                repo_name: "r".to_string(),
                local_service_count: 1,
                peer_repo_count: 0,
            },
            stats: ScanStats {
                endpoints: 0,
                calls: 0,
            },
            findings: vec![],
            delta: None,
            verified: vec![],
            graphql: GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
            baseline: None,
            checked: None,
            functions: None,
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("run_id").is_none());
        assert_eq!(v["head_sha"], "a".repeat(40));
        assert_eq!(v["delta"], serde_json::Value::Null);
    }

    /// carrick#2284: a payload with neither member is byte for byte what it
    /// was before they existed, and with both it carries them in the shape
    /// the cloud reads, each optional part written only when it has something
    /// to say.
    #[test]
    fn baseline_and_checked_are_written_only_when_the_run_states_them() {
        let mut payload = PrResultPayload {
            repo: "r".to_string(),
            pr_number: 1,
            head_sha: None,
            run_id: None,
            topology: Topology {
                repo_name: "r".to_string(),
                local_service_count: 1,
                peer_repo_count: 0,
            },
            stats: ScanStats {
                endpoints: 0,
                calls: 0,
            },
            findings: vec![],
            delta: None,
            verified: vec![],
            graphql: GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
            baseline: None,
            checked: None,
            functions: None,
        };
        const BARE: &str = r#"{"repo":"r","pr_number":1,"head_sha":null,"topology":{"repo_name":"r","local_service_count":1,"peer_repo_count":0},"stats":{"endpoints":0,"calls":0},"findings":[],"delta":null,"verified":[],"graphql":{"libraries":[],"operations_indexed":false},"has_types":true}"#;
        assert_eq!(serde_json::to_string(&payload).unwrap(), BARE);

        payload.baseline = Some(Baseline {
            base_sha: Some("b".repeat(40)),
            copy: BaselineCopy::Current,
            main_index: vec![
                MainIndexEntry {
                    service: Some("api".to_string()),
                    commit: "c".repeat(40),
                    scanner_version: Some("0.3.113".to_string()),
                    dirty: true,
                },
                MainIndexEntry {
                    service: None,
                    commit: "d".repeat(40),
                    scanner_version: None,
                    dirty: false,
                },
            ],
            peers: PeerIndexes {
                count: 12,
                sha256: "e".repeat(64),
            },
        });
        payload.checked = Some(Checked {
            pairs: PairCounts {
                compatible: 22,
                unverifiable: 5,
                incompatible: 1,
                not_compared: 3,
            },
            sdk_pairs: Some(PairCounts {
                compatible: 4,
                unverifiable: 0,
                incompatible: 1,
                not_compared: 2,
            }),
            dependencies: DependencyCounts {
                shared: 41,
                changed: Some(2),
            },
        });
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            v["baseline"],
            json!({
                "base_sha": "b".repeat(40),
                "copy": "current",
                "main_index": [
                    { "service": "api", "commit": "c".repeat(40), "scanner_version": "0.3.113", "dirty": true },
                    { "service": null, "commit": "d".repeat(40) },
                ],
                "peers": { "count": 12, "sha256": "e".repeat(64) },
            })
        );
        assert_eq!(
            v["checked"],
            json!({
                "pairs": { "compatible": 22, "unverifiable": 5, "incompatible": 1, "not_compared": 3 },
                "sdk_pairs": { "compatible": 4, "unverifiable": 0, "incompatible": 1, "not_compared": 2 },
                "dependencies": { "shared": 41, "changed": 2 },
            })
        );

        let Some(baseline) = &mut payload.baseline else {
            unreachable!()
        };
        baseline.base_sha = None;
        baseline.copy = BaselineCopy::None;
        let Some(checked) = &mut payload.checked else {
            unreachable!()
        };
        checked.dependencies.changed = None;
        checked.sdk_pairs = None;
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v["checked"].get("sdk_pairs").is_none(), "{v:#}");
        assert_eq!(v["baseline"]["base_sha"], serde_json::Value::Null);
        assert_eq!(v["baseline"]["copy"], "none");
        assert_eq!(v["checked"]["dependencies"], json!({ "shared": 41 }));
        for (copy, word) in [
            (BaselineCopy::Stale, "stale"),
            (BaselineCopy::OtherScanner, "other_scanner"),
        ] {
            assert_eq!(serde_json::to_value(copy).unwrap(), json!(word));
        }
    }

    /// The wire spellings cloud#599 codes against, pinned exactly: a renderer
    /// that reads `edge_source == "fact"` must never meet `"Fact"`.
    #[test]
    fn source_fields_carry_their_wire_spellings() {
        use crate::agents::file_analyzer_agent::ResolutionSource;

        let finding = Finding::type_mismatch(
            "GET",
            "/api/users/:id",
            None,
            vec!["server.ts:50".into()],
            "User",
            "UserV2",
            "incompatible",
        )
        .with_edge_source(EdgeSource::fold([Some(ResolutionSource::Model)]))
        .with_verdict_state(Some(VerdictState::Resolved));
        let v = serde_json::to_value(&finding).unwrap();
        assert_eq!(v["edge_source"], "candidate");
        assert_eq!(v["verdict_state"], "resolved");

        let finding = Finding::method_mismatch(
            "GET",
            "/api/orders",
            None,
            vec!["client.ts:12".into()],
            "POST",
        )
        .with_edge_source(EdgeSource::fold([Some(ResolutionSource::FileBasedRoute)]))
        .with_verdict_state(Some(VerdictState::NotChecked));
        let v = serde_json::to_value(&finding).unwrap();
        assert_eq!(v["edge_source"], "fact");
        assert_eq!(v["verdict_state"], "not_checked");

        let v = serde_json::to_value(
            Finding::type_mismatch("GET", "/a", None, vec![], "A", "B", "d")
                .with_verdict_state(Some(VerdictState::Unresolved)),
        )
        .unwrap();
        assert_eq!(v["verdict_state"], "unresolved");
    }

    /// A finding the scan cannot place carries neither field, so a payload
    /// from this scanner is byte-identical to a 0.3.41 one for that row and
    /// the cloud's "not stated" branch keeps today's behaviour.
    #[test]
    fn unstated_source_fields_are_omitted() {
        let v = serde_json::to_value(Finding::type_mismatch(
            "GET",
            "/a",
            None,
            vec![],
            "A",
            "B",
            "d",
        ))
        .unwrap();
        assert!(v.get("edge_source").is_none(), "got: {v}");
        assert!(v.get("verdict_state").is_none(), "got: {v}");

        let v = serde_json::to_value(Finding::method_mismatch("GET", "/a", None, vec![], "POST"))
            .unwrap();
        assert!(v.get("edge_source").is_none(), "got: {v}");
        assert!(v.get("verdict_state").is_none(), "got: {v}");
    }

    /// A pairing is no better than its weakest row.
    #[test]
    fn edge_source_folds_over_every_contributing_row() {
        use crate::agents::file_analyzer_agent::ResolutionSource::{
            FileBasedRoute, ImportedMember, InlineLiteral, Model,
        };

        assert_eq!(
            EdgeSource::fold([Some(FileBasedRoute), Some(ImportedMember)]),
            Some(EdgeSource::Fact)
        );
        // The literal states the path; whether the row exists at all is the
        // model's answer (#332), so the pairing is a candidate, as the cloud
        // reads it (carrick#1642).
        assert_eq!(
            EdgeSource::fold([Some(FileBasedRoute), Some(InlineLiteral)]),
            Some(EdgeSource::Candidate)
        );
        assert_eq!(
            EdgeSource::fold([Some(FileBasedRoute), Some(Model)]),
            Some(EdgeSource::Candidate),
            "one model row makes the whole pairing a candidate"
        );
        // A model row still decides even when another row said nothing.
        assert_eq!(
            EdgeSource::fold([None, Some(Model)]),
            Some(EdgeSource::Candidate)
        );
        assert_eq!(
            EdgeSource::fold([Some(FileBasedRoute), None]),
            None,
            "a row that stated no source cannot be claimed as a fact"
        );
        assert_eq!(EdgeSource::fold([]), None, "no rows, nothing to state");
    }

    /// carrick#3091: the pass-named sources of GraphQL, socket and bus rows are
    /// facts; one model row among them still makes the pairing a candidate.
    #[test]
    fn protocol_pass_sources_fold_to_fact() {
        use crate::agents::file_analyzer_agent::ResolutionSource::{
            EventBus, GraphqlDocument, GraphqlSchema, Model, SocketCall,
        };

        for pair in [
            [Some(GraphqlSchema), Some(GraphqlDocument)],
            [Some(SocketCall), Some(SocketCall)],
            [Some(EventBus), Some(EventBus)],
        ] {
            assert_eq!(EdgeSource::fold(pair), Some(EdgeSource::Fact));
        }
        assert_eq!(
            EdgeSource::fold([Some(GraphqlSchema), Some(Model)]),
            Some(EdgeSource::Candidate)
        );
    }

    /// `view_module` rides an orphaned endpoint only when true, so the common
    /// row is unchanged.
    #[test]
    fn view_module_rides_the_orphan_row_only_when_true() {
        let v =
            serde_json::to_value(Finding::orphaned_endpoint("GET", "/dashboard", None)).unwrap();
        assert!(v.get("view_module").is_none(), "got: {v}");

        let v = serde_json::to_value(
            Finding::orphaned_endpoint("GET", "/dashboard", None).with_view_module(true),
        )
        .unwrap();
        assert_eq!(v["view_module"], true);
    }

    /// Every builder is a no-op on a kind that carries no such field, so a
    /// caller cannot silently attach one to the wrong finding.
    #[test]
    fn source_builders_are_no_ops_on_other_kinds() {
        let finding = Finding::missing_endpoint("GET", "/a", None, vec![])
            .with_edge_source(Some(EdgeSource::Candidate))
            .with_verdict_state(Some(VerdictState::Unresolved))
            .with_view_module(true);
        assert_eq!(
            finding,
            Finding::missing_endpoint("GET", "/a", None, vec![])
        );
    }

    fn mismatch(
        source: Option<EdgeSource>,
        verdict: Option<VerdictState>,
        on_main: Option<bool>,
    ) -> Finding {
        let mut f = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", "boom")
            .with_edge_source(source)
            .with_verdict_state(verdict);
        f.set_on_main(on_main);
        f
    }

    fn removal(
        source: Option<EdgeSource>,
        verdict: Option<VerdictState>,
        on_main: Option<bool>,
        unknown: Option<OnMainUnknown>,
    ) -> Removal {
        Removal {
            removed: RemovedSide {
                method: "GET".into(),
                path: "/x".into(),
                service: None,
                place: "a.ts:1".into(),
            },
            edge_source: source,
            verdict_state: verdict,
            on_main,
            on_main_unknown: unknown,
        }
    }

    #[test]
    fn break_status_fact_resolved_introduced_is_certain() {
        let f = mismatch(
            Some(EdgeSource::Fact),
            Some(VerdictState::Resolved),
            Some(false),
        );
        assert_eq!(f.break_status(), BreakStatus::Certain);
        let verb = Finding::method_mismatch("PUT", "/x", None, vec![], "POST")
            .with_edge_source(Some(EdgeSource::Fact))
            .with_verdict_state(Some(VerdictState::NotChecked));
        let mut verb = verb;
        verb.set_on_main(Some(false));
        assert_eq!(verb.break_status(), BreakStatus::Certain);
    }

    #[test]
    fn break_status_main_already_had_it() {
        let f = mismatch(Some(EdgeSource::Fact), None, Some(true));
        assert_eq!(f.break_status(), BreakStatus::OnMain);
    }

    #[test]
    fn break_status_fact_with_main_unknown_is_possible() {
        let mut f = mismatch(Some(EdgeSource::Fact), None, None);
        f.set_on_main_unknown(OnMainUnknown::MainIndexStale);
        assert_eq!(
            f.break_status(),
            BreakStatus::Possible {
                reason: "main_index_stale"
            }
        );
    }

    #[test]
    fn break_status_candidate_edge_is_a_candidate_even_when_introduced() {
        let f = mismatch(Some(EdgeSource::Candidate), None, Some(false));
        assert!(matches!(f.break_status(), BreakStatus::Candidate { .. }));
    }

    #[test]
    fn break_status_mismatch_with_an_unresolved_side_is_a_candidate() {
        let f = mismatch(
            Some(EdgeSource::Fact),
            Some(VerdictState::Unresolved),
            Some(false),
        );
        assert_eq!(
            f.break_status(),
            BreakStatus::Candidate {
                reason: "type_side_unresolved"
            }
        );
    }

    /// An SDK-mediated mismatch states no source by design and is enforced.
    #[test]
    fn break_status_sdk_mediated_mismatch_with_no_source_is_certain() {
        let f = mismatch(None, None, Some(false));
        assert_eq!(f.break_status(), BreakStatus::Certain);
    }

    #[test]
    fn break_status_orphaned_removal_fact_introduced_is_certain() {
        let f = Finding::orphaned_endpoint("CLIENT->SERVER", "evt", None).with_removal(
            removal(
                Some(EdgeSource::Fact),
                Some(VerdictState::NotChecked),
                Some(false),
                None,
            ),
            vec![],
        );
        assert_eq!(f.break_status(), BreakStatus::Certain);
        let g = Finding::missing_endpoint("GET", "/x", None, vec![]).with_removal(
            removal(
                Some(EdgeSource::Fact),
                Some(VerdictState::NotChecked),
                Some(false),
                None,
            ),
            vec![],
        );
        assert_eq!(g.break_status(), BreakStatus::Certain);
    }

    #[test]
    fn break_status_removal_without_proof_is_a_candidate() {
        let unmarked = Finding::missing_endpoint("GET", "/x", None, vec![])
            .with_removal(removal(Some(EdgeSource::Fact), None, None, None), vec![]);
        assert!(matches!(
            unmarked.break_status(),
            BreakStatus::Candidate { .. }
        ));
        let nosource = Finding::missing_endpoint("GET", "/x", None, vec![])
            .with_removal(removal(None, None, Some(false), None), vec![]);
        assert!(matches!(
            nosource.break_status(),
            BreakStatus::Candidate { .. }
        ));
        let unknown = Finding::missing_endpoint("GET", "/x", None, vec![]).with_removal(
            removal(
                Some(EdgeSource::Fact),
                None,
                None,
                Some(OnMainUnknown::RemovalUnplaced),
            ),
            vec![],
        );
        assert_eq!(
            unknown.break_status(),
            BreakStatus::Possible {
                reason: "removal_unplaced"
            }
        );
    }

    #[test]
    fn break_status_gaps_and_advisories_are_not_breaks() {
        assert_eq!(
            Finding::missing_endpoint("GET", "/x", None, vec![]).break_status(),
            BreakStatus::NotABreak
        );
        assert_eq!(
            Finding::orphaned_endpoint("GET", "/x", None).break_status(),
            BreakStatus::NotABreak
        );
        assert_eq!(
            Finding::env_var_call("GET", "/x", "API", vec![]).break_status(),
            BreakStatus::NotABreak
        );
    }

    #[test]
    fn an_unread_reason_has_the_pr_checks_words() {
        assert_eq!(
            FileNotReadReason::DoesNotParse.phrase(),
            Some("it does not parse")
        );
        assert_eq!(
            FileNotReadReason::NotUtf8Text.phrase(),
            Some("it is not UTF-8 text")
        );
        assert_eq!(FileNotReadReason::OperationsMissing.phrase(), None);
    }

    /// The break answer rides on the wire (plan B1): every finding writes
    /// `break`, and `break_reason` only when the status carries one.
    #[test]
    fn every_finding_carries_its_break_on_the_wire() {
        let cases: Vec<(Finding, &str, Option<&str>)> = vec![
            (
                mismatch(
                    Some(EdgeSource::Fact),
                    Some(VerdictState::Resolved),
                    Some(false),
                ),
                "certain",
                None,
            ),
            (
                {
                    let mut f = mismatch(Some(EdgeSource::Fact), None, None);
                    f.set_on_main_unknown(OnMainUnknown::MainIndexStale);
                    f
                },
                "possible",
                Some("main_index_stale"),
            ),
            (
                mismatch(
                    Some(EdgeSource::Fact),
                    Some(VerdictState::Unresolved),
                    Some(false),
                ),
                "candidate",
                Some("type_side_unresolved"),
            ),
            (
                mismatch(Some(EdgeSource::Fact), None, Some(true)),
                "on_main",
                None,
            ),
            (
                Finding::env_var_call("GET", "/x", "API", vec![]),
                "none",
                None,
            ),
            (
                Finding::orphaned_endpoint("CLIENT->SERVER", "evt", None).with_removal(
                    removal(
                        Some(EdgeSource::Fact),
                        Some(VerdictState::NotChecked),
                        Some(false),
                        None,
                    ),
                    vec![],
                ),
                "certain",
                None,
            ),
        ];
        for (finding, want, reason) in cases {
            let v = serde_json::to_value(&finding).unwrap();
            assert_eq!(v["break"], json!(want), "got: {v}");
            match reason {
                Some(reason) => assert_eq!(v["break_reason"], json!(reason), "got: {v}"),
                None => assert!(
                    !v.as_object().unwrap().contains_key("break_reason"),
                    "got: {v}"
                ),
            }
        }
    }
}
