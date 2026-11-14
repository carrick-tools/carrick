//! Path-matching primitives shared by every Carrick surface that compares
//! HTTP route paths, plus the evidence-kind classification of what a matched
//! pair means ([`classify_relationship`]).
//!
//! Extracted verbatim from the scanner's `MountGraph` (whose methods now
//! delegate here) so the scanner and, via the optional `wasm` feature,
//! Node.js consumers run the exact same matching semantics. Everything is a
//! pure function; the default native build has zero dependencies (the
//! optional `serde` feature adds derives on the classification enums).
//!
//! Two questions, two functions (#378/#381):
//!
//! - [`paths_match`] answers ROUTING truth: "does this route pattern cover
//!   this call path?" A catch-all `/*` covers everything; that is what a
//!   router would do with the request.
//! - [`match_agreement`] answers PAIRING strength: how many literal segments
//!   the two sides actually agree on. Params and wildcards cover segments
//!   without vouching for them, so they contribute nothing. A pairing with
//!   zero agreement (a wildcard-only producer absorbing an arbitrary call)
//!   carries no signal and must not be reported as a cross-repo match; among
//!   several matching producers, the ones with maximal agreement are the
//!   real candidates (a catch-all never beats a concrete route).
//!
//! `paths_match` is defined as `match_agreement(..).is_some()`, so the two
//! can never disagree about WHETHER a pair matches.
//!
//! Neither of those two is the question a REPORTING surface asks, which is
//! "may I show this pair as a match, and if not why" — [`match_verdict`] and
//! its `Option<u32>` form [`reportable_agreement`] answer that one, and every
//! surface (scanner, cloud, MCP) should call them rather than re-deriving the
//! rule from `paths_match` (#537). They fold in the consumer-side check
//! [`is_unknown_call_path`]: a call path with no literal segment is an
//! extraction gap, not a call to every route, so no producer may claim it.
//!
//! The `wasm` feature (default off) adds wasm-bindgen exports for
//! [`paths_match`], [`is_param_segment`], [`route_identity`], [`match_agreement`],
//! [`path_literal_specificity`], [`is_catch_all_path`], [`catch_all_prefix`],
//! [`is_wildcard_segment`], [`literal_segments`],
//! [`is_unknown_call_path`], [`match_verdict`], [`reportable_agreement`],
//! [`classify_relationship`], [`method_matches`], and the body-dispatch trio [`dispatch_key`],
//! [`dispatch_verdict`], [`match_verdict_with_dispatch`] /
//! [`reportable_agreement_with_dispatch`], and the pairing pair
//! [`PairingPool`] / [`canonical_path`]:
//!
//! ```text
//! cargo build --target wasm32-unknown-unknown -p carrick-match --features wasm
//! wasm-bindgen --target nodejs --out-dir <dir> \
//!     target/wasm32-unknown-unknown/<profile>/carrick_match.wasm
//! ```
//!
//! Socket and pub/sub rows pair by exact key, and [`names_pair`] decides
//! whether two rows that share a key may pair, given where each row's name
//! means something ([`NameScope`], carrick#1663).
//!
//! Which producer a call reaches, for every protocol, is one function:
//! [`PairingPool::pair`] (and its one-shot form [`pair_winners`],
//! carrick#3496). It folds the primitives above, `names_pair` included, and
//! the `wasm` feature exports it as the class `PairingPool` (JSON in, JSON
//! out) beside [`canonical_path`], so a surface pairs through it rather than
//! re-composing the primitives. A call that names a procedure
//! ([`ProcedureKey`]) pairs first, by exact key. Its rule is the vector file
//! `tests/fixtures/pairing.vectors.json`, run natively and over wasm.

mod name_scope;
mod pairing;

pub use name_scope::{GLOBAL_SCOPE, NameScope, SERVICE_SCOPE, ScopedRow, names_pair};
pub use pairing::{
    Call, PairOutcome, Pairing, PairingPool, ProcedureKey, Producer, Routed, RouterId, Winner,
    canonical_path, pair_winners,
};

/// The kind of source evidence backing one side of a match: a route
/// definition (a server declaring the operation) or a call site (a client
/// invoking it).
///
/// Every match side is backed by one of these two. The distinction is what
/// separates a real producer/consumer relationship from two clients that
/// merely encode the same externally-hosted contract (#379).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MatchEvidence {
    /// The side declares/serves the operation (a route definition, an SDL
    /// root field, a socket listener, a pub/sub subscriber).
    #[default]
    RouteDefinition,
    /// The side invokes the operation (an outbound call expression). An
    /// index entry that sits on the producer side but was derived from a
    /// call expression carries this kind.
    CallSite,
}

/// What a matched pair of operations means, classified purely by the
/// evidence kind on each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MatchRelationship {
    /// One side defines the route, the other calls it: a genuine
    /// producer→consumer contract edge.
    ProducerConsumer,
    /// Both sides are call sites: nobody in the pair defines the route, so
    /// the operations encode the same contract served outside the index.
    /// Producer/consumer roles do not apply to this relationship.
    SharedExternalContract,
}

/// Classify what a matched pair means from the evidence kind of each side.
///
/// A pair is a producer/consumer edge only when the producer-side entry is
/// backed by a route definition. When the producer-side evidence is itself a
/// call site, the "producer" role would be fabricated from a call expression
/// (#379): both sides merely encode the same external contract.
///
/// The consumer side is accepted for symmetry (matchers know both sides);
/// today's matchers always pass `CallSite` there, and its value does not
/// change the classification — only the producer side's evidence decides.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn classify_relationship(
    producer_side: MatchEvidence,
    _consumer_side: MatchEvidence,
) -> MatchRelationship {
    match producer_side {
        MatchEvidence::RouteDefinition => MatchRelationship::ProducerConsumer,
        MatchEvidence::CallSite => MatchRelationship::SharedExternalContract,
    }
}

/// Whether a producer route path matches a consumer call path.
///
/// `endpoint_path` is the declared route on the producer side (it may carry
/// `:param`/`{param}`/`<param>`/`[param]` placeholders, trailing `?` optional
/// markers, and `*`/`**`/`(.*)` wildcards). `call_path` is the canonical
/// consumer call path. Matches on exact equality, on parameter segments
/// (symmetric: a placeholder on either side matches the other side), or on
/// wildcard patterns (trailing wildcards are prefix matches; a mid-path `*`
/// matches exactly one segment).
///
/// This is ROUTING truth only. Whether the pair is worth reporting as a
/// cross-repo match is a separate question — see [`match_agreement`].
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn paths_match(endpoint_path: &str, call_path: &str) -> bool {
    match_agreement(endpoint_path, call_path).is_some()
}

/// How strongly a producer route and a consumer call path agree, or `None`
/// if they do not match at all.
///
/// The agreement score is the number of segment positions where BOTH sides
/// carry the same literal text. Placeholders (`:id`, `{id}`, …) and
/// wildcards (`*`, `**`, `(.*)`) cover segments without vouching for them,
/// so they contribute 0 — a param is a declared one-segment placeholder, and
/// a trailing catch-all swallows an unbounded tail the producer asserts
/// nothing about. For a trailing-wildcard match only the literal segments of
/// the prefix count.
///
/// `Some(0)` therefore means "routes, but with no corroborating signal":
/// `GET /*` vs anything, `/:slug` vs `/about`. Surfaces must not report such
/// pairs as matches (#381); they are routing/absorption only. Relative use:
/// among all producers matching one call, pair the call with the
/// maximal-agreement producers — a catch-all (`/api/**`, agreement 1) never
/// outranks a concrete route (`/api/v1/chat/new`, agreement 4).
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn match_agreement(endpoint_path: &str, call_path: &str) -> Option<u32> {
    let endpoint_segments = segments(endpoint_path);
    let call_segments = segments(call_path);
    agreement(
        &SplitPath::new(endpoint_path, &endpoint_segments),
        &SplitPath::new(call_path, &call_segments),
    )
}

/// A path and its `/`-separated segments, split once. Every matcher below
/// reads the segments from here, so a surface that compares one path with
/// many (the [`PairingPool`]) splits each path once rather than once per
/// comparison (the pool holds `Box<str>` segments; a one-shot comparison
/// borrows `&str` ones).
#[derive(Debug)]
pub(crate) struct SplitPath<'a, S> {
    text: &'a str,
    segments: &'a [S],
}

impl<'a, S: AsRef<str>> SplitPath<'a, S> {
    /// `segments` must be `split_route(text)`, in order.
    pub(crate) fn new(text: &'a str, segments: &'a [S]) -> Self {
        SplitPath { text, segments }
    }

    pub(crate) fn segments(&self) -> &'a [S] {
        self.segments
    }
}

/// [`split_route`], borrowed: a one-shot comparison's segments.
pub(crate) fn segments(path: &str) -> Vec<&str> {
    split_route(path).collect()
}

/// `path.split('/')`, except a trailing optional wildcard group `{/*name}`
/// (path-to-regexp v8, Express 5) splits as the segment `*name` after its
/// prefix: `/proxy{/*splat}` reads as `/proxy/*splat`, never as the literals
/// `proxy{` and `*splat}`. The matcher's `/*name` already covers the bare
/// prefix, so the optional form reads the same as `/*name` (carrick#4205).
/// Every split of a route path (comparison, pool, literal count) goes
/// through here.
pub(crate) fn split_route(path: &str) -> impl Iterator<Item = &str> {
    let (head, tail) = match optional_wildcard_tail(path) {
        Some(open) => {
            let above = &path[..open];
            (
                above.strip_suffix('/').unwrap_or(above),
                Some(&path[open + 2..path.len() - 1]),
            )
        }
        None => (path, None),
    };
    head.split('/').chain(tail)
}

/// Byte offset of a trailing `{/*name}` group, when the path ends in one.
fn optional_wildcard_tail(path: &str) -> Option<usize> {
    let body = path.strip_suffix('}')?;
    let open = body.rfind("{/*")?;
    is_wildcard_name(&body[open + 3..]).then_some(open)
}

/// [`split_route`], owned: the segments a [`PairingPool`] holds per route.
pub(crate) fn owned_segments(path: &str) -> Box<[Box<str>]> {
    split_route(path).map(Box::from).collect()
}

/// [`match_agreement`] over paths already split.
pub(crate) fn agreement<E: AsRef<str>, C: AsRef<str>>(
    endpoint: &SplitPath<'_, E>,
    call: &SplitPath<'_, C>,
) -> Option<u32> {
    // Exact match: every literal segment is shared by construction.
    if endpoint.text == call.text {
        return Some(literal_specificity(endpoint.segments));
    }

    // Parameter matching (e.g., /users/:id matches /users/123), then
    // wildcards — same dispatch order the boolean matcher always had.
    agreement_with_params(endpoint.segments, call.segments)
        .or_else(|| agreement_with_wildcards(endpoint, call))
}

/// Number of LITERAL segments in a path: segments that are real text rather
/// than a placeholder (`:id`, `{id}`, `<id>`, `[id]`), a wildcard (`*`,
/// `**`, `(.*)`, a named `*name` / `{*name}`), or empty/whitespace. A
/// trailing `{/*name}` group splits as `*name` ([`split_route`]). This is the most agreement any pairing
/// with this path could ever produce — 0 means the path can never corroborate
/// a match (`/*`, `/**`, `/:param`), i.e. it is routing infrastructure, not a
/// concrete contract.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn path_literal_specificity(path: &str) -> u32 {
    literal_count(split_route(path))
}

/// [`path_literal_specificity`] over a path already split.
pub(crate) fn literal_specificity<S: AsRef<str>>(segments: &[S]) -> u32 {
    literal_count(segments.iter().map(AsRef::as_ref))
}

fn literal_count<'s>(segments: impl Iterator<Item = &'s str>) -> u32 {
    segments
        .filter(|seg| is_literal_segment(seg.trim_end_matches('?')))
        .count() as u32
}

/// Whether a producer route ends in a catch-all wildcard (`/*`, `/**`,
/// `/(.*)`, or a named `/*name`, `/{*name}`, `{/*name}`) — the patterns the
/// matcher treats as unbounded prefix matches. See [`catch_all_prefix`].
/// Such a route is a mount or fallback: it absorbs calls by design, so
/// "no consumer matched it" (orphaned) is not a meaningful observation.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn is_catch_all_path(path: &str) -> bool {
    catch_all_prefix(path).is_some()
}

/// The one answer to "is this route a catch-all, and what prefix does it
/// cover" (carrick#4205): the part of the route before its wildcard, or
/// `None` when the route is not a catch-all. Every catch-all reader
/// ([`is_catch_all_path`], the matcher, [`catch_all_covers_call_literal`],
/// the scanner's mount read) takes its prefix from here.
///
/// Forms, all as the LAST part of the path:
/// - `/*`, `/**`, `/(.*)`, trimmed exactly as the matcher always trimmed them
///   (`/a/*/**` trims to `/a`);
/// - the path-to-regexp v8 (Express 5) named forms `/*name` and `/{*name}`;
/// - the optional group `{/*name}` (`/proxy{/*splat}` covers `/proxy`).
///
/// A name is `[A-Za-z_$][A-Za-z0-9_$]*`; anything else (`/files/*.js`, a
/// quoted name) stays literal. A mid-path `*name` is not a catch-all: it
/// matches one segment, like a mid-path `*`. Stored paths are never
/// rewritten; this is a read-time answer, so `route_identity` still tells
/// `/v1/*` and `/v1/*rest` apart.
pub fn catch_all_prefix(path: &str) -> Option<&str> {
    if path.ends_with("/*") || path.ends_with("/**") {
        return Some(path.trim_end_matches("/**").trim_end_matches("/*"));
    }
    if path.ends_with("/(.*)") {
        return Some(path.trim_end_matches("/(.*)"));
    }
    if let Some(open) = optional_wildcard_tail(path) {
        let above = &path[..open];
        return Some(above.strip_suffix('/').unwrap_or(above));
    }
    let (above, last) = path.rsplit_once('/')?;
    is_named_wildcard(last).then_some(above)
}

/// [`catch_all_prefix`] over wasm: a JS string, or `undefined` when the
/// route is not a catch-all.
#[cfg(feature = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = catch_all_prefix)]
pub fn catch_all_prefix_owned(path: &str) -> Option<String> {
    catch_all_prefix(path).map(str::to_string)
}

/// The literal segments of a route path, in order: what is left after
/// [`split_route`] drops placeholders, wildcards and empty segments. The
/// cloud's near-miss text reads this rather than splitting a path itself
/// (`/proxy{/*splat}` -> `["proxy"]`).
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn literal_segments(path: &str) -> Vec<String> {
    split_route(path)
        .filter(|seg| is_literal_segment(seg.trim_end_matches('?')))
        .map(str::to_string)
        .collect()
}

/// `[A-Za-z_$][A-Za-z0-9_$]*`: the name a path-to-regexp v8 wildcard takes.
fn is_wildcard_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// `*name` or `{*name}`: a path-to-regexp v8 named wildcard segment.
fn is_named_wildcard(seg: &str) -> bool {
    let inner = seg
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(seg);
    inner.strip_prefix('*').is_some_and(is_wildcard_name)
}

/// Whether `endpoint_path` is a catch-all ([`catch_all_prefix`]) that routes
/// `call_path` AND the call states a literal segment inside the wildcard's
/// tail (carrick#4186). Such a route says nothing about that segment, so its
/// handler's type is a generic envelope, not the contract of the call's path:
/// the edge stands (routing truth) but the type check must not judge the pair.
///
/// A placeholder in the tail does not count: `/packets/**` vs `/packets/:enc`
/// states no literal the route fails to cover, so that pair is still
/// compared. The prefix is [`catch_all_prefix`], the one the matcher reads.
///
/// Not a wasm export and read by nothing that decides routing: it changes no
/// part of `paths_match`, `match_agreement` or `match_verdict`; only the type
/// pairing reads it.
pub fn catch_all_covers_call_literal(endpoint_path: &str, call_path: &str) -> bool {
    let Some(prefix) = catch_all_prefix(endpoint_path) else {
        return false;
    };
    if match_agreement(endpoint_path, call_path).is_none() {
        return false;
    }
    path_literal_specificity(call_path) > path_literal_specificity(prefix)
}

/// Whether a CONSUMER call path names nothing the index can join on: it has
/// no literal segment at all. That is the empty string, `/`, a bare or
/// leading-slash wildcard (`*`, `/*`, `/**`, `/(.*)`), and a path that
/// normalised down to placeholders only (`:base/:rest`, the shape a
/// template-literal URL whose every segment was interpolated collapses to).
///
/// Such a call is not a call to "any route" — it is a call whose path the
/// scanner failed to resolve. Routing it anywhere fabricates an edge, so it
/// must stay unmatched (#537). This is the consumer-side counterpart of
/// [`is_catch_all_path`]: a catch-all is a producer deliberately covering a
/// subtree; an unknown call path is an extraction gap.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn is_unknown_call_path(call_path: &str) -> bool {
    path_literal_specificity(call_path) == 0
}

/// The method a producer registers to answer EVERY method on its path
/// (`app.all("/graphql", handler)`). A producer-side label only: no request
/// is ever sent with it.
pub const ANY_METHOD: &str = "ALL";

/// Whether a producer registered under `producer_method` serves a consumer
/// call made with `consumer_method` (carrick#1148).
///
/// Methods compare case-insensitively. A producer registered with
/// [`ANY_METHOD`] serves every method, the way the router dispatches it. The
/// relation is not symmetric: a consumer never sends `ALL`, so a consumer
/// `ALL` matches only a producer `ALL`.
///
/// Every surface that pairs a call with a route by method calls this rather
/// than comparing the strings, so an any-method route pairs the same way in
/// the scanner and in the cloud.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn method_matches(producer_method: &str, consumer_method: &str) -> bool {
    let producer = producer_method.trim();
    producer.eq_ignore_ascii_case(consumer_method.trim())
        || producer.eq_ignore_ascii_case(ANY_METHOD)
}

/// Why a producer route and a consumer call are, or are not, a reportable
/// match. Returned by [`match_verdict`]; the named rejections are what a
/// surface shows instead of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MatchVerdict {
    /// The pair routes AND agrees on at least one literal segment. The only
    /// verdict a surface may report as a match.
    Matched,
    /// The consumer call path carries no literal segment
    /// ([`is_unknown_call_path`]): the scanner never resolved where this call
    /// goes, so no producer may claim it.
    UnknownCallPath,
    /// The producer route is a bare catch-all (`/*`, `/**`, `/(.*)`) with no
    /// literal prefix. It absorbs every path by design and corroborates none.
    CatchAllRoute,
    /// The two paths do not route to each other at all.
    NoRouteMatch,
    /// The pair routes but shares no literal segment — a prefixed catch-all or
    /// a param-only route covering the call without vouching for it.
    NoLiteralAgreement,
    /// The producer is one case of a body-dispatching handler and the call
    /// states no value for the field it switches on (carrick#831). The paths
    /// route, but the route is not the whole operation identity here: without
    /// the call's own value, the pair is a guess between the producer's
    /// siblings, so no edge may be drawn to this case. The call still reaches
    /// the route: [`dispatch_outcome`] keeps it one edge to the route, with the
    /// case unknown (carrick#2048).
    DispatchValueUnknown,
    /// The producer is one case of a body-dispatching handler and the call
    /// states a DIFFERENT value for that field. This is a clean negative, not
    /// a gap: the call names a sibling operation on the same route, and the
    /// sibling is where its edge belongs.
    DispatchValueMismatch,
}

/// What the CALL states for the field a dispatching producer switches on.
///
/// Returned by [`dispatch_verdict`]; folded into [`MatchVerdict`] by
/// [`match_verdict_with_dispatch`]. Keep the two in step: every non-`Matched`
/// variant here has to name a rejection there, or a surface that reads the
/// folded verdict loses a distinction this one draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DispatchVerdict {
    /// The producer states no dispatch: an ordinary route, whose identity is
    /// method and path alone. A call carrying a dispatch value still matches
    /// it — the value is then a body field like any other, not part of the
    /// operation's identity, and only the PRODUCER can make it one.
    NotDispatching,
    /// Both sides name the same (location, field) and the same value.
    Matched,
    /// The producer dispatches and the call states nothing the producer's
    /// field can be read from: no dispatch at all, or one on a different
    /// field.
    ValueUnknown,
    /// Both sides name the same (location, field) and DIFFERENT values.
    ValueMismatch,
}

/// The canonical key text for one dispatch case: `"body:action=upload-logs"`.
///
/// This is the string that makes nine operations behind one route nine keys
/// (carrick#831). Every surface that keys, dedupes or counts a dispatching
/// operation appends it to the `METHOD:path` key it already builds, so a
/// bookkeeping map cannot fold the nine back into one.
///
/// Inputs are taken verbatim: the value is the literal the source states, and
/// no case folding or trimming is applied, because a dispatch value is a
/// string compared for equality by the handler itself.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn dispatch_key(location: &str, field: &str, value: &str) -> String {
    format!("{location}:{field}={value}")
}

/// The case a [`dispatch_key`] states, in the few characters a listing can
/// carry: `" {action=upload-logs}"` from `"body:action=upload-logs"`. The
/// location is dropped and the leading space is part of the label, so it
/// appends straight to an operation. A key with no location is taken whole.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn dispatch_label(key: &str) -> String {
    let case = key.split_once(':').map_or(key, |(_, case)| case);
    format!(" {{{case}}}")
}

/// Whether a call satisfies the dispatch case a producer answers.
///
/// Both sides are passed as [`dispatch_key`] strings, or `None` where the row
/// carries no dispatch. `None` on the producer is the ordinary route and ends
/// the question ([`DispatchVerdict::NotDispatching`]).
///
/// The comparison is by (location, field) first and value second, so a call
/// that states a value for a DIFFERENT field is `ValueUnknown` rather than a
/// mismatch: it says nothing at all about the field this producer switches
/// on, and "unknown" is the honest reading of silence.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn dispatch_verdict(
    producer_dispatch: Option<String>,
    call_dispatch: Option<String>,
) -> DispatchVerdict {
    let Some(producer) = producer_dispatch else {
        return DispatchVerdict::NotDispatching;
    };
    let Some(call) = call_dispatch else {
        return DispatchVerdict::ValueUnknown;
    };
    let (Some((producer_field, producer_value)), Some((call_field, call_value))) =
        (producer.split_once('='), call.split_once('='))
    else {
        // A key without a value separator states no value. Treat it as
        // silence rather than as a value that happens to be the whole string.
        return DispatchVerdict::ValueUnknown;
    };
    if producer_field != call_field {
        return DispatchVerdict::ValueUnknown;
    }
    if producer_value == call_value {
        DispatchVerdict::Matched
    } else {
        DispatchVerdict::ValueMismatch
    }
}

/// [`match_verdict`] with the producer's dispatch case folded in: the one
/// decision a surface asks about a pair when the producer may be one case of a
/// body-dispatching handler (carrick#831).
///
/// The path question is asked FIRST and unchanged. A dispatch case is a
/// narrowing of an operation that already has to route: a call that does not
/// reach the path is not a dispatch failure, and naming it one would hide the
/// real reason behind a newer one.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn match_verdict_with_dispatch(
    endpoint_path: &str,
    call_path: &str,
    producer_dispatch: Option<String>,
    call_dispatch: Option<String>,
) -> MatchVerdict {
    fold_dispatch(
        match_verdict(endpoint_path, call_path),
        producer_dispatch,
        call_dispatch,
    )
}

/// The path verdict `path`, narrowed by the dispatch case: the second half of
/// [`match_verdict_with_dispatch`].
pub(crate) fn fold_dispatch(
    path: MatchVerdict,
    producer_dispatch: Option<String>,
    call_dispatch: Option<String>,
) -> MatchVerdict {
    match path {
        MatchVerdict::Matched => match dispatch_verdict(producer_dispatch, call_dispatch) {
            DispatchVerdict::NotDispatching | DispatchVerdict::Matched => MatchVerdict::Matched,
            DispatchVerdict::ValueUnknown => MatchVerdict::DispatchValueUnknown,
            DispatchVerdict::ValueMismatch => MatchVerdict::DispatchValueMismatch,
        },
        rejected => rejected,
    }
}

/// What one call is to the producers its path routes it to, once each of
/// them has answered [`match_verdict_with_dispatch`] (carrick#2048).
///
/// The per-pair verdict says whether a call is one CASE of a dispatching
/// route. This answers the question a surface asks about the call as a whole:
/// whether it has an edge, to what, and whether it is missing a producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DispatchOutcome {
    /// At least one producer answered `Matched`: an edge to each of them. A
    /// dispatching sibling that answered `DispatchValueUnknown` is not one.
    Matched,
    /// None matched, and at least one producer is a dispatching route whose
    /// case the call does not state. The route exists and the call reaches
    /// it; only the case is unknown. One edge to the route, with the case
    /// unknown, and never a missing endpoint.
    RouteCaseUnknown,
    /// None matched, and every dispatching producer answered
    /// `DispatchValueMismatch`: the call states a value no case answers. The
    /// call is unmatched.
    NoCaseAnswers,
    /// None matched, and the dispatch question declined nothing: the path
    /// verdicts did.
    NotMatched,
}

/// [`DispatchOutcome`] from the counts of one call's per-producer verdicts:
/// `Matched`, `DispatchValueUnknown` and `DispatchValueMismatch`. Counts, not
/// a list, so the wasm surface takes three numbers.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn dispatch_outcome(matched: u32, case_unknown: u32, case_mismatched: u32) -> DispatchOutcome {
    if matched > 0 {
        DispatchOutcome::Matched
    } else if case_unknown > 0 {
        DispatchOutcome::RouteCaseUnknown
    } else if case_mismatched > 0 {
        DispatchOutcome::NoCaseAnswers
    } else {
        DispatchOutcome::NotMatched
    }
}

/// [`reportable_agreement`] with the producer's dispatch case folded in.
///
/// The agreement number is the PATH's, unchanged: a dispatch case narrows
/// which operation a call names, it does not add a segment the two sides
/// agree on. Ranking several producers against one call therefore still
/// ranks on path specificity, and among the cases of one dispatching route
/// at most one survives this function at all.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn reportable_agreement_with_dispatch(
    endpoint_path: &str,
    call_path: &str,
    producer_dispatch: Option<String>,
    call_dispatch: Option<String>,
) -> Option<u32> {
    match match_verdict_with_dispatch(endpoint_path, call_path, producer_dispatch, call_dispatch) {
        MatchVerdict::Matched => match_agreement(endpoint_path, call_path),
        _ => None,
    }
}

/// The single decision every surface asks of the matcher: is this
/// producer/consumer pair reportable, and if not, why.
///
/// The rejections are ordered so the reason names the consumer's own failure
/// first: an unresolved call path is an extraction gap regardless of which
/// producer happened to be offered, and it is checked before anything about
/// the route.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn match_verdict(endpoint_path: &str, call_path: &str) -> MatchVerdict {
    let endpoint_segments = segments(endpoint_path);
    let call_segments = segments(call_path);
    let endpoint = SplitPath::new(endpoint_path, &endpoint_segments);
    let call = SplitPath::new(call_path, &call_segments);
    path_verdict(&endpoint, &call, || agreement(&endpoint, &call))
}

/// [`match_verdict`] over paths already split, with the pair's
/// [`match_agreement`] supplied by the caller (asked only once both
/// rejections that precede it have passed), so a caller that has already
/// scored the pair does not score it again.
pub(crate) fn path_verdict<E: AsRef<str>, C: AsRef<str>>(
    endpoint: &SplitPath<'_, E>,
    call: &SplitPath<'_, C>,
    agreement: impl FnOnce() -> Option<u32>,
) -> MatchVerdict {
    if literal_specificity(call.segments) == 0 {
        return MatchVerdict::UnknownCallPath;
    }
    if is_catch_all_path(endpoint.text) && literal_specificity(endpoint.segments) == 0 {
        return MatchVerdict::CatchAllRoute;
    }
    match agreement() {
        None => MatchVerdict::NoRouteMatch,
        Some(0) => MatchVerdict::NoLiteralAgreement,
        Some(_) => MatchVerdict::Matched,
    }
}

/// The literal agreement of a pair a surface is allowed to report as a match,
/// or `None` when it is not — see [`match_verdict`] for which of the four
/// rejections applied.
///
/// This is the function surfaces should call. [`match_agreement`] answers the
/// narrower question of how much two paths agree and returns `Some(0)` for
/// pairs that route but corroborate nothing; reading that as a match is the
/// bug this wrapper exists to make impossible.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn reportable_agreement(endpoint_path: &str, call_path: &str) -> Option<u32> {
    match match_verdict(endpoint_path, call_path) {
        MatchVerdict::Matched => match_agreement(endpoint_path, call_path),
        _ => None,
    }
}

/// A segment that carries literal, comparable text: non-empty after
/// whitespace trim, not a route parameter, not a wildcard token.
fn is_literal_segment(seg: &str) -> bool {
    !seg.trim().is_empty() && !is_param_segment(seg) && !is_wildcard_segment(seg)
}

/// Whether one route segment is a wildcard token: `*`, `**`, `(.*)`, or a
/// path-to-regexp v8 named wildcard `*name` / `{*name}`. A wildcard is never
/// a literal (it counts toward no agreement). Split a path with
/// [`literal_segments`] rather than `split('/')`: a trailing `{/*name}` group
/// is one wildcard, not two text segments.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn is_wildcard_segment(seg: &str) -> bool {
    seg == "*" || seg == "**" || seg == "(.*)" || is_named_wildcard(seg)
}

/// Segment-by-segment matching with route parameters (on either side) and
/// trailing `?` optional segments on the endpoint side. Returns the literal
/// agreement count, or `None` if the paths don't match.
fn agreement_with_params<E: AsRef<str>, C: AsRef<str>>(
    endpoint_segments: &[E],
    call_segments: &[C],
) -> Option<u32> {
    // Handle optional segments (e.g., /users/:id?)
    let endpoint_required_count = endpoint_segments
        .iter()
        .filter(|s| !s.as_ref().ends_with('?'))
        .count();

    // Call must have at least required segments and at most all segments
    if call_segments.len() < endpoint_required_count
        || call_segments.len() > endpoint_segments.len()
    {
        return None;
    }

    let mut agreement = 0u32;
    for (i, endpoint_seg) in endpoint_segments.iter().map(AsRef::as_ref).enumerate() {
        // Check if this is an optional segment
        let is_optional = endpoint_seg.ends_with('?');
        let seg = endpoint_seg.trim_end_matches('?');

        // If we're past the call segments, remaining endpoint segments must be optional
        if i >= call_segments.len() {
            if !is_optional {
                return None;
            }
            continue;
        }

        let call_seg = call_segments[i].as_ref();

        // A path parameter on EITHER side matches the other side. This must be
        // symmetric: a caller URL with a variable normalizes to a `:param` segment
        // and must match a concrete provider segment, just as a caller's concrete
        // value must match a provider's `:param`. We also accept the other param
        // syntaxes (`{id}`, `<id>`, `[id]`) so that routes captured verbatim from
        // frameworks that don't use Express-style colons still match cross-repo.
        if is_param_segment(seg) || is_param_segment(call_seg) {
            // A param stands in for a real path value; an empty or
            // whitespace-only segment on the other side is not one. Without
            // this guard a route indexed with a stray blank segment (#332,
            // "/api/users/ ") reads as satisfying "/api/users/:userId".
            if seg.trim().is_empty() || call_seg.trim().is_empty() {
                return None;
            }
            // A param covers the segment without vouching for it: no agreement.
            continue;
        }

        // Exact match required for non-parameter segments
        if seg != call_seg {
            return None;
        }

        if is_literal_segment(seg) {
            agreement += 1;
        }
    }

    Some(agreement)
}

/// Returns true if a single path segment is a route parameter placeholder in any
/// of the common syntaxes: `:id` (Express/path-to-regexp), `{id}` (OpenAPI/Fastify),
/// `<id>` (Flask-style), or `[id]`/`[...id]` (Next.js dynamic segments).
/// Also the param definition `canonical_path_has_literal_segment` (#307)
/// reuses, so the noise gate and the matcher agree on what a placeholder is.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn is_param_segment(seg: &str) -> bool {
    seg.starts_with(':')
        || (seg.starts_with('{') && seg.ends_with('}'))
        || (seg.starts_with('<') && seg.ends_with('>'))
        || (seg.starts_with('[') && seg.ends_with(']'))
}

/// True when `seg` is exactly one template hole, `${...}`, spanning the whole
/// segment (nested braces inside the hole are balanced).
fn is_whole_template_hole(seg: &str) -> bool {
    let Some(inner) = seg.strip_prefix("${") else {
        return false;
    };
    let mut depth = 1usize;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + c.len_utf8() == inner.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// A route path's identity with every parameter spelling folded to one token.
///
/// A whole segment that [`is_param_segment`] accepts, or that is exactly one
/// `${...}` template hole, becomes `:` with its name dropped; a trailing `?`
/// stays on the token. Every other segment (literals, mixed segments such as
/// `v${n}`, wildcards) is kept as written, case included.
///
/// This answers identity only (does a PR's delta list one route twice?). It
/// is not a matcher: routing stays with [`paths_match`], and a `${...}` hole
/// is not a parameter there.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn route_identity(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let (body, optional) = match seg.strip_suffix('?') {
                Some(body) => (body, "?"),
                None => (seg, ""),
            };
            if is_param_segment(body) || is_whole_template_hole(body) {
                format!(":{optional}")
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Match paths with wildcard patterns, returning the literal agreement count.
///
/// Match semantics as implemented (kept byte-identical to the scanner's
/// original boolean matcher):
/// - An endpoint ending in a catch-all ([`catch_all_prefix`]: `/*`, `/**`,
///   `/(.*)`, `/*name`, `/{*name}`, `{/*name}`) is a prefix match: the
///   call path only has to start with the part before the wildcard. A
///   trailing `/*` therefore matches any number of segments, exactly like
///   `/**`, not just one. Agreement is the literal specificity of the
///   prefix — the wildcard-covered tail vouches for nothing.
/// - Anywhere else, a `*` or named `*name` segment matches exactly one call
///   segment (path-to-regexp v8 lets a mid-path `*name` span several; this
///   reads it as one, so it never pairs a call v8 would refuse), and both
///   paths must have the same number of segments. Agreement counts the
///   literal positions.
fn agreement_with_wildcards<E: AsRef<str>, C: AsRef<str>>(
    endpoint: &SplitPath<'_, E>,
    call: &SplitPath<'_, C>,
) -> Option<u32> {
    let (endpoint_path, call_path) = (endpoint.text, call.text);
    // A catch-all prefix only matches on a segment boundary: the call path is
    // the prefix itself, or continues with '/'. A raw byte-prefix check would
    // let `/api/**` claim `/api2/...`.
    fn prefix_matches_on_boundary(prefix: &str, call_path: &str) -> bool {
        match call_path.strip_prefix(prefix) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        }
    }

    if let Some(prefix) = catch_all_prefix(endpoint_path) {
        return prefix_matches_on_boundary(prefix, call_path)
            .then(|| path_literal_specificity(prefix));
    }

    // Check for single-segment wildcards in the middle
    if endpoint.segments.len() != call.segments.len() {
        return None;
    }

    let mut agreement = 0u32;
    let pairs = endpoint.segments.iter().zip(call.segments.iter());
    for (endpoint_seg, call_seg) in pairs.map(|(e, c)| (e.as_ref(), c.as_ref())) {
        if endpoint_seg == "*" || is_named_wildcard(endpoint_seg) {
            continue; // Single-segment wildcard matches anything
        }
        if endpoint_seg != call_seg {
            return None;
        }
        if is_literal_segment(endpoint_seg) {
            agreement += 1;
        }
    }

    Some(agreement)
}

/// True if `host` exactly equals, or is a subdomain of, one of `domains`
/// (carrick#3441: the one implementation of the internal/external host rule).
///
/// Comparison is case-insensitive because DNS hostnames are. Entries may be
/// bare hosts (`api.example.com`) or full URLs (`https://api.example.com`);
/// both reduce to a bare host. The host's own port is ignored. There is
/// deliberately no substring matching: host `company` must not match a
/// configured `api.company.com`, and `api.com` must not match it either.
pub fn host_matches_domains<S: AsRef<str>>(
    host: &str,
    domains: impl IntoIterator<Item = S>,
) -> bool {
    let host_without_port = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    domains.into_iter().any(|domain| {
        let domain = domain_host(domain.as_ref());
        host_without_port == domain || host_without_port.ends_with(&format!(".{domain}"))
    })
}

/// Reduce a configured domain entry (`carrick.json` `internalDomains` /
/// `externalDomains`) to a bare, comparable lowercase hostname: no scheme,
/// path, query or port.
pub fn domain_host(domain: &str) -> String {
    let lower = domain.to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    host.split(':').next().unwrap_or(host).to_string()
}

/// The route of a call whose literal `http(s)://` host a project declares
/// internal, else `None` (carrick#3441).
///
/// `call_path` is a call target as stored (`https://api.example.com/jobs/run`).
/// When its host equals, or is a subdomain of, an entry of `internal_domains`
/// (the union of every service's `carrick.json` `internalDomains`), the call
/// is this system's own and is compared by its path: the result is the route
/// (`/jobs/run`) with query and fragment dropped, `/` when there is none.
/// Anything else is `None`: relative paths, an undeclared host, a lookalike
/// (`api.example.com.evil`, `evil-api.example.com`), a host that is an
/// interpolation (`${host}`), and `user@host` forms whose real host is the
/// part after the `@`. An undeclared host is a third party, never guessed
/// internal (carrick-cloud#656).
///
/// HTTP only: other protocols pair by name, not by host.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn internal_origin_path(call_path: &str, internal_domains: Vec<String>) -> Option<String> {
    let trimmed = call_path.trim_matches(|c| c == '`' || c == '"' || c == '\'');
    let lower_prefix = trimmed.get(..8).map(str::to_ascii_lowercase);
    let rest = match lower_prefix.as_deref() {
        Some("https://") => &trimmed[8..],
        _ => match trimmed.get(..7).map(str::to_ascii_lowercase).as_deref() {
            Some("http://") => &trimmed[7..],
            _ => return None,
        },
    };
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() || host.contains('$') || host.contains('{') {
        return None;
    }
    if !host_matches_domains(host, &internal_domains) {
        return None;
    }
    let route = tail.split(['?', '#']).next().unwrap_or("");
    Some(if route.is_empty() {
        "/".to_string()
    } else {
        route.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The params arm over two unsplit paths.
    fn agreement_with_params(endpoint_path: &str, call_path: &str) -> Option<u32> {
        super::agreement_with_params(&segments(endpoint_path), &segments(call_path))
    }

    /// The wildcard arm over two unsplit paths.
    fn agreement_with_wildcards(endpoint_path: &str, call_path: &str) -> Option<u32> {
        let (endpoint_segments, call_segments) = (segments(endpoint_path), segments(call_path));
        super::agreement_with_wildcards(
            &SplitPath::new(endpoint_path, &endpoint_segments),
            &SplitPath::new(call_path, &call_segments),
        )
    }

    // --- internal origin (carrick#3441) -------------------------------------

    fn internal(call: &str, domains: &[&str]) -> Option<String> {
        internal_origin_path(call, domains.iter().map(|d| d.to_string()).collect())
    }

    #[test]
    fn a_declared_host_reduces_to_its_route() {
        let d = ["api.ledger.example"];
        assert_eq!(
            internal("https://api.ledger.example/jobs/run", &d).as_deref(),
            Some("/jobs/run")
        );
        assert_eq!(
            internal("http://api.ledger.example:8080/jobs/status?x=1#f", &d).as_deref(),
            Some("/jobs/status")
        );
        assert_eq!(
            internal("https://API.Ledger.Example/jobs", &d).as_deref(),
            Some("/jobs")
        );
        assert_eq!(
            internal("https://api.ledger.example", &d).as_deref(),
            Some("/")
        );
    }

    #[test]
    fn subdomains_and_url_shaped_entries_match() {
        assert_eq!(
            internal("https://eu.api.ledger.example/x", &["api.ledger.example"]).as_deref(),
            Some("/x")
        );
        assert_eq!(
            internal(
                "https://api.ledger.example/x",
                &["https://api.ledger.example"]
            )
            .as_deref(),
            Some("/x")
        );
        assert_eq!(
            internal(
                "https://api.ledger.example/x",
                &["HTTPS://API.ledger.example/v1"]
            )
            .as_deref(),
            Some("/x")
        );
    }

    #[test]
    fn lookalikes_undeclared_and_unreadable_hosts_do_not_match() {
        let d = ["api.ledger.example"];
        assert_eq!(internal("https://api.ledger.example.evil/x", &d), None);
        assert_eq!(internal("https://evil-api.ledger.example/x", &d), None);
        assert_eq!(internal("https://api.ledger.example@evil.com/x", &d), None);
        assert_eq!(internal("https://api.mailer.example/v1/send", &d), None);
        assert_eq!(internal("https://${host}/x", &d), None);
        assert_eq!(internal("/jobs/run", &d), None);
        assert_eq!(internal("https://api.ledger.example/x", &[]), None);
    }

    // --- methods (carrick#1148) --------------------------------------------

    #[test]
    fn an_any_method_producer_serves_every_consumer_method() {
        for method in ["GET", "post", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(method_matches("ALL", method), "ALL must serve {method}");
        }
        assert!(method_matches("all", "GET"));
    }

    #[test]
    fn methods_otherwise_compare_case_insensitively_and_one_way() {
        assert!(method_matches("get", "GET"));
        assert!(!method_matches("GET", "POST"));
        // A consumer never sends ALL, so it does not widen a producer.
        assert!(!method_matches("POST", "ALL"));
        assert!(method_matches("ALL", "ALL"));
    }

    // --- body dispatch (carrick#831) ---------------------------------------

    #[test]
    fn dispatch_label_is_the_case_without_its_location() {
        assert_eq!(
            dispatch_label(&dispatch_key("body", "action", "get-type-reach")),
            " {action=get-type-reach}"
        );
        assert_eq!(dispatch_label("meta.kind=x"), " {meta.kind=x}");
    }

    #[test]
    fn dispatch_key_is_the_case_written_verbatim() {
        assert_eq!(
            dispatch_key("body", "action", "search-by-intent"),
            "body:action=search-by-intent"
        );
        // A header case and a body case on the same field are different keys:
        // the location is part of what the handler switches on.
        assert_ne!(
            dispatch_key("body", "action", "x"),
            dispatch_key("header", "action", "x")
        );
    }

    #[test]
    fn a_plain_route_is_never_narrowed_by_the_call() {
        // Only the producer can make a field part of the operation's identity.
        assert_eq!(
            dispatch_verdict(None, Some(dispatch_key("body", "action", "x"))),
            DispatchVerdict::NotDispatching
        );
        assert_eq!(
            dispatch_verdict(None, None),
            DispatchVerdict::NotDispatching
        );
        assert_eq!(
            match_verdict_with_dispatch(
                "/types/check-or-upload",
                "/types/check-or-upload",
                None,
                Some(dispatch_key("body", "action", "upload-logs")),
            ),
            MatchVerdict::Matched
        );
    }

    #[test]
    fn a_dispatching_producer_matches_only_its_own_case() {
        let producer = Some(dispatch_key("body", "action", "search-by-intent"));
        assert_eq!(
            dispatch_verdict(producer.clone(), producer.clone()),
            DispatchVerdict::Matched
        );
        assert_eq!(
            dispatch_verdict(
                producer.clone(),
                Some(dispatch_key("body", "action", "upload-logs"))
            ),
            DispatchVerdict::ValueMismatch
        );
        // A value for a different field says nothing about this one.
        assert_eq!(
            dispatch_verdict(
                producer.clone(),
                Some(dispatch_key("body", "kind", "search-by-intent"))
            ),
            DispatchVerdict::ValueUnknown
        );
        // Location is part of the field identity.
        assert_eq!(
            dispatch_verdict(
                producer.clone(),
                Some(dispatch_key("header", "action", "search-by-intent"))
            ),
            DispatchVerdict::ValueUnknown
        );
    }

    #[test]
    fn a_call_stating_no_value_stays_unmatched() {
        let producer = Some(dispatch_key("body", "action", "search-by-intent"));
        assert_eq!(
            dispatch_verdict(producer.clone(), None),
            DispatchVerdict::ValueUnknown
        );
        assert_eq!(
            match_verdict_with_dispatch(
                "/types/check-or-upload",
                "/types/check-or-upload",
                producer.clone(),
                None,
            ),
            MatchVerdict::DispatchValueUnknown
        );
        assert_eq!(
            reportable_agreement_with_dispatch(
                "/types/check-or-upload",
                "/types/check-or-upload",
                producer,
                None,
            ),
            None
        );
    }

    /// carrick#2048: a call that reaches a dispatching route without stating
    /// its case keeps an edge to the route; only a value no case answers, or
    /// a path no producer routes, leaves it without one.
    #[test]
    fn a_call_whose_case_is_unknown_keeps_its_route() {
        assert_eq!(dispatch_outcome(1, 3, 0), DispatchOutcome::Matched);
        assert_eq!(dispatch_outcome(0, 3, 0), DispatchOutcome::RouteCaseUnknown);
        // A case on another field, which the call states nothing about, is
        // still a case the call may reach.
        assert_eq!(dispatch_outcome(0, 1, 2), DispatchOutcome::RouteCaseUnknown);
        assert_eq!(dispatch_outcome(0, 0, 3), DispatchOutcome::NoCaseAnswers);
        assert_eq!(dispatch_outcome(0, 0, 0), DispatchOutcome::NotMatched);
    }

    #[test]
    fn the_path_question_is_answered_first() {
        // A call that never reaches the route is a route failure, not a
        // dispatch one: naming it a dispatch failure would hide the real
        // reason behind a newer one.
        assert_eq!(
            match_verdict_with_dispatch(
                "/types/check-or-upload",
                "/types/other",
                Some(dispatch_key("body", "action", "x")),
                None,
            ),
            MatchVerdict::NoRouteMatch
        );
        assert_eq!(
            match_verdict_with_dispatch(
                "/*",
                "/types/check-or-upload",
                Some(dispatch_key("body", "action", "x")),
                None,
            ),
            MatchVerdict::CatchAllRoute
        );
    }

    #[test]
    fn a_matching_case_keeps_the_paths_own_agreement() {
        let case = Some(dispatch_key("body", "action", "search-by-intent"));
        assert_eq!(
            reportable_agreement_with_dispatch(
                "/types/check-or-upload",
                "/types/check-or-upload",
                case.clone(),
                case,
            ),
            reportable_agreement("/types/check-or-upload", "/types/check-or-upload")
        );
    }

    #[test]
    fn test_paths_match_dispatch() {
        // Exact match
        assert!(paths_match("/users", "/users"));
        // Parameter matching (e.g., /users/:id matches /users/123)
        assert!(paths_match("/users/:id", "/users/123"));
        // Wildcard matching
        assert!(paths_match("/api/**", "/api/users/123/orders"));
        // No match
        assert!(!paths_match("/users", "/orders"));
        assert!(!paths_match("/users/:id", "/users/123/orders"));
    }

    #[test]
    fn test_path_matches_with_optional_segments() {
        // Test optional segment matching
        assert!(agreement_with_params("/users/:id?", "/users").is_some());
        assert!(agreement_with_params("/users/:id?", "/users/123").is_some());
        assert!(agreement_with_params("/users/:id", "/users").is_none()); // Not optional
    }

    #[test]
    fn test_path_matches_with_params_is_symmetric() {
        // A param on the endpoint side matches a concrete caller segment.
        assert!(agreement_with_params("/users/:id", "/users/123").is_some());
        // ...and a param on the caller side (a normalized `${id}` interpolation) matches
        // a concrete provider segment. This direction previously failed.
        assert!(agreement_with_params("/users/123", "/users/:id").is_some());
        // Params on both sides match.
        assert!(agreement_with_params("/users/:id", "/users/:userId").is_some());
    }

    #[test]
    fn test_path_matches_with_params_across_syntaxes() {
        // A caller normalized to `:id` must match a provider route captured verbatim in
        // OpenAPI/Fastify (`{id}`), Flask (`<id>`), or Next.js (`[id]`) syntax.
        assert!(agreement_with_params("/users/{id}", "/users/:id").is_some());
        assert!(agreement_with_params("/users/<id>", "/users/:id").is_some());
        assert!(agreement_with_params("/users/[id]", "/users/:id").is_some());
        // ...and against a concrete value.
        assert!(agreement_with_params("/users/{id}", "/users/123").is_some());
        // Non-param segments must still match exactly.
        assert!(agreement_with_params("/users/{id}", "/orders/123").is_none());
    }

    #[test]
    fn test_path_matches_with_wildcards() {
        // Test wildcard matching
        assert!(agreement_with_wildcards("/api/*", "/api/users").is_some());
        assert!(agreement_with_wildcards("/api/**", "/api/users/123/orders").is_some());
        assert!(agreement_with_wildcards("/files/(.*)", "/files/path/to/file.txt").is_some());

        // Regression: file-based routing synthesizes Next.js catch-all routes
        // (`app/files/[...slug]/route.ts`) as `/files/**`, which must match
        // single- and multi-segment caller URLs.
        assert!(agreement_with_wildcards("/files/**", "/files/foo").is_some());
        assert!(agreement_with_wildcards("/files/**", "/files/foo/bar").is_some());
        // carrick#4205: a named catch-all (`*slug`, the Express 5 spelling)
        // is a catch-all too, not a literal.
        assert_eq!(
            agreement_with_wildcards("/files/*slug", "/files/foo"),
            Some(1)
        );
        assert_eq!(
            agreement_with_wildcards("/files/*slug", "/files/foo/bar"),
            Some(1)
        );
    }

    #[test]
    fn test_param_never_matches_empty_or_whitespace_segment() {
        // Regression (#333): an endpoint whose full_path carried a trailing
        // whitespace segment ("/api/users/ ", see #332) matched the consumer
        // call "/api/users/:userId" because a param on either side skipped the
        // comparison entirely. A param stands in for a real path value, and an
        // empty or whitespace segment is not one.
        assert!(agreement_with_params("/api/users/ ", "/api/users/:userId").is_none());
        assert!(agreement_with_params("/api/users/:userId", "/api/users/ ").is_none());
        assert!(agreement_with_params("/api/users/", "/api/users/:userId").is_none());
        assert!(!paths_match("/api/users/ ", "/api/users/:userId"));
        // Real values still match a param on the other side.
        assert!(agreement_with_params("/api/users/:id", "/api/users/123").is_some());
    }

    #[test]
    fn test_classify_relationship_by_evidence_kind() {
        // A route definition on the producer side is a real producer/consumer
        // contract edge, regardless of the consumer side's evidence.
        assert_eq!(
            classify_relationship(MatchEvidence::RouteDefinition, MatchEvidence::CallSite),
            MatchRelationship::ProducerConsumer
        );
        // Producer-side evidence that is itself a call site means nobody in
        // the pair defines the route: the pair is two encodings of the same
        // external contract, never a fabricated producer role (#379).
        assert_eq!(
            classify_relationship(MatchEvidence::CallSite, MatchEvidence::CallSite),
            MatchRelationship::SharedExternalContract
        );
    }

    #[test]
    fn test_match_evidence_default_is_route_definition() {
        // Index entries recorded before evidence kinds existed deserialize to
        // the default; a producer-side entry defaults to a route definition so
        // pre-existing matches keep their producer/consumer classification.
        assert_eq!(MatchEvidence::default(), MatchEvidence::RouteDefinition);
    }

    #[test]
    fn test_route_identity_folds_parameter_spellings() {
        let one = route_identity("/a/:id/b");
        for same in [
            "/a/{id}/b",
            "/a/${id}/b",
            "/a/${encodeURIComponent(id)}/b",
            "/a/<id>/b",
            "/a/[id]/b",
            "/a/[...id]/b",
            "/a/:other/b",
        ] {
            assert_eq!(route_identity(same), one, "{same}");
        }
        assert_eq!(route_identity("/a/:id?"), "/a/:?");
        assert_eq!(route_identity("/a/{id}?"), route_identity("/a/:id?"));
        assert_ne!(route_identity("/a/:id?"), route_identity("/a/:id"));
        for different in [
            "/a/v${n}/b",
            "/a/${x}-${y}/b",
            "/a/${x}${y}/b",
            "/a/foo{bar}baz/b",
            "/a/me/b",
            "/A/:id/b",
            "/a/*/b",
        ] {
            assert_ne!(route_identity(different), one, "{different}");
        }
    }

    #[test]
    fn test_is_param_segment_syntaxes() {
        assert!(is_param_segment(":id"));
        assert!(is_param_segment("{id}"));
        assert!(is_param_segment("<id>"));
        assert!(is_param_segment("[id]"));
        assert!(is_param_segment("[...slug]"));
        assert!(!is_param_segment("users"));
        assert!(!is_param_segment("*"));
        assert!(!is_param_segment("v${n}"));
    }

    // ---- #378/#381: agreement scoring ----

    /// `paths_match` is defined over `match_agreement`, so any matching pair
    /// must score, and any non-matching pair must not.
    #[test]
    fn match_agreement_is_some_iff_paths_match() {
        let cases = [
            ("/users", "/users"),
            ("/users/:id", "/users/123"),
            ("/api/**", "/api/users/123/orders"),
            ("/*", "/anything/at/all"),
            ("/users", "/orders"),
            ("/users/:id", "/users/123/orders"),
            ("/files/*slug", "/files/foo"),
        ];
        for (endpoint, call) in cases {
            assert_eq!(
                paths_match(endpoint, call),
                match_agreement(endpoint, call).is_some(),
                "paths_match and match_agreement disagree on ({endpoint}, {call})"
            );
        }
    }

    #[test]
    fn match_agreement_counts_shared_literal_segments() {
        // Exact literal paths: every segment agrees.
        assert_eq!(match_agreement("/oauth/token", "/oauth/token"), Some(2));
        assert_eq!(match_agreement("/nodes", "/nodes"), Some(1));
        assert_eq!(
            match_agreement("/api/v1/chat/new", "/api/v1/chat/new"),
            Some(4)
        );
        // Params cover a segment without vouching for it.
        assert_eq!(match_agreement("/users/:id", "/users/123"), Some(1));
        assert_eq!(match_agreement("/users/123", "/users/:id"), Some(1));
        assert_eq!(match_agreement("/users/:id", "/users/:userId"), Some(1));
        // Mid-path `*` likewise.
        assert_eq!(match_agreement("/api/*/users", "/api/v2/users"), Some(2));
        // Non-matching pairs score nothing.
        assert_eq!(match_agreement("/users", "/orders"), None);
    }

    /// #381: wildcard-only producers carry no agreement signal — the pair
    /// routes (`Some`) but corroborates nothing (`0`).
    #[test]
    fn wildcard_only_producers_have_zero_agreement() {
        assert_eq!(match_agreement("/*", "/repos/acme/widgets"), Some(0));
        assert_eq!(match_agreement("/**", "/internal/metrics/export"), Some(0));
        assert_eq!(match_agreement("/(.*)", "/spa/route"), Some(0));
        // A root param route is the same class: nothing literal to agree on.
        assert_eq!(match_agreement("/:slug", "/about"), Some(0));
        // Root-to-root: nothing literal either.
        assert_eq!(match_agreement("/", "/"), Some(0));
    }

    /// #381: a catch-all with a literal mount prefix DOES carry the prefix's
    /// signal — but only the prefix's. The swallowed tail vouches for nothing,
    /// so a concrete route over the same call always outranks it.
    #[test]
    fn trailing_wildcard_agreement_is_prefix_only() {
        assert_eq!(match_agreement("/api/**", "/api/v1/chat/new"), Some(1));
        assert_eq!(match_agreement("/api/*", "/api/v1/chat/new"), Some(1));
        assert_eq!(match_agreement("/files/(.*)", "/files/a/b/c"), Some(1));
        let concrete = match_agreement("/api/v1/chat/new", "/api/v1/chat/new").unwrap();
        let catch_all = match_agreement("/api/**", "/api/v1/chat/new").unwrap();
        assert!(
            concrete > catch_all,
            "a concrete producer must outrank a catch-all for the same call"
        );
    }

    #[test]
    fn test_path_literal_specificity() {
        assert_eq!(path_literal_specificity("/"), 0);
        assert_eq!(path_literal_specificity("/*"), 0);
        assert_eq!(path_literal_specificity("/**"), 0);
        assert_eq!(path_literal_specificity("/(.*)"), 0);
        assert_eq!(path_literal_specificity("/:slug"), 0);
        assert_eq!(path_literal_specificity("/nodes"), 1);
        assert_eq!(path_literal_specificity("/api/**"), 1);
        assert_eq!(path_literal_specificity("/oauth/token"), 2);
        assert_eq!(path_literal_specificity("/users/:id/orders"), 2);
        assert_eq!(path_literal_specificity("/api/v1/chat/new"), 4);
        // Optional marker doesn't change what's literal.
        assert_eq!(path_literal_specificity("/users/:id?"), 1);
    }

    #[test]
    fn catch_all_prefix_requires_segment_boundary() {
        // /api/** must not claim /api2/... (raw byte-prefix bug)
        assert_eq!(match_agreement("/api/**", "/api2/users"), None);
        assert_eq!(match_agreement("/api/**", "/api/users"), Some(1));
        assert_eq!(match_agreement("/api/**", "/api"), Some(1));
        assert_eq!(match_agreement("/files/(.*)", "/filesystem/a"), None);
    }

    #[test]
    fn test_is_catch_all_path() {
        assert!(is_catch_all_path("/*"));
        assert!(is_catch_all_path("/**"));
        assert!(is_catch_all_path("/(.*)"));
        assert!(is_catch_all_path("/api/**"));
        assert!(is_catch_all_path("/files/(.*)"));
        assert!(!is_catch_all_path("/api/v1/chat/new"));
        assert!(!is_catch_all_path("/users/:id"));
        assert!(is_catch_all_path("/files/*slug"));
        // Mid-path single-segment wildcard is not a catch-all.
        assert!(!is_catch_all_path("/api/*/users"));
    }

    /// carrick#4205: the path-to-regexp v8 (Express 5) named wildcards are
    /// catch-alls, and `catch_all_prefix` is what each one covers.
    #[test]
    fn named_wildcards_are_catch_alls_with_their_prefix() {
        let catch_alls = [
            ("/v1/*rest", "/v1"),
            ("/*splat", ""),
            ("/{*splat}", ""),
            ("/files/{*path}", "/files"),
            ("/proxy{/*splat}", "/proxy"),
            ("/proxy/{/*splat}", "/proxy"),
            ("/{/*path}", ""),
            ("/v1/*", "/v1"),
            ("/v1/**", "/v1"),
            ("/v1/(.*)", "/v1"),
            ("/*", ""),
        ];
        for (path, prefix) in catch_alls {
            assert!(is_catch_all_path(path), "{path} is a catch-all");
            assert_eq!(catch_all_prefix(path), Some(prefix), "prefix of {path}");
        }
        for path in [
            "/files/*.js",
            "/a/*x/b",
            "/users/:id",
            "/v1/*1x",
            "/v1/{*}",
            "/v1{/*}",
            "/v1/rest*",
            "/x{/:id}",
        ] {
            assert!(!is_catch_all_path(path), "{path} is not a catch-all");
            assert_eq!(catch_all_prefix(path), None, "{path} has no prefix");
        }
    }

    #[test]
    fn named_wildcards_count_no_literal_and_fold_the_optional_group() {
        assert_eq!(path_literal_specificity("/proxy{/*splat}"), 1);
        assert_eq!(path_literal_specificity("/*splat"), 0);
        assert_eq!(path_literal_specificity("/{*splat}"), 0);
        assert_eq!(path_literal_specificity("/{/*path}"), 0);
        assert_eq!(path_literal_specificity("/v1/*rest"), 1);
        assert_eq!(match_verdict("/*splat", "/x"), MatchVerdict::CatchAllRoute);
        assert_eq!(
            match_verdict("/{*splat}", "/x"),
            MatchVerdict::CatchAllRoute
        );
        assert_eq!(
            match_verdict("/{/*path}", "/x"),
            MatchVerdict::CatchAllRoute
        );
        assert_eq!(literal_segments("/proxy{/*splat}"), vec!["proxy"]);
        assert_eq!(literal_segments("/v1/*splat"), vec!["v1"]);
        assert_eq!(literal_segments("/v1/:id/{*rest}"), vec!["v1"]);
        assert!(is_wildcard_segment("*rest"));
        assert!(is_wildcard_segment("{*rest}"));
        assert!(!is_wildcard_segment("*.js"));
    }

    #[test]
    fn named_catch_alls_pair_like_the_unnamed_ones() {
        assert_eq!(reportable_agreement("/v1/*rest", "/v1/users/7"), Some(1));
        assert_eq!(reportable_agreement("/v1/{*rest}", "/v1/users/7"), Some(1));
        assert_eq!(reportable_agreement("/proxy{/*splat}", "/proxy"), Some(1));
        assert_eq!(
            reportable_agreement("/proxy{/*splat}", "/proxy/a/b"),
            Some(1)
        );
        // R3: `/v1/*rest` covers the bare prefix, exactly as `/v1/*` does.
        assert_eq!(reportable_agreement("/v1/*rest", "/v1"), Some(1));
        assert_eq!(reportable_agreement("/v1/*", "/v1"), Some(1));
        // The prefix ends on a segment boundary.
        assert_eq!(reportable_agreement("/v1/*rest", "/v10/x"), None);
        assert_eq!(reportable_agreement("/proxy{/*splat}", "/proxyx/a"), None);
        // A mid-path named wildcard matches one segment, like a mid-path `*`.
        assert_eq!(match_agreement("/a/*x/b", "/a/1/b"), Some(2));
        assert_eq!(match_agreement("/a/*x/b", "/a/1/2/b"), None);
        // A glob is literal text, not a wildcard.
        assert_eq!(match_agreement("/files/*.js", "/files/app.js"), None);
        assert_eq!(match_agreement("/files/*.js", "/files/*.js"), Some(2));
    }

    /// carrick#4215 over the named forms: a catch-all covering a call literal
    /// takes its prefix from `catch_all_prefix`.
    #[test]
    fn named_catch_all_covering_a_call_literal() {
        assert!(catch_all_covers_call_literal("/v1/*rest", "/v1/jobs/open"));
        assert!(catch_all_covers_call_literal(
            "/v1{/*rest}",
            "/v1/jobs/open"
        ));
        assert!(catch_all_covers_call_literal("/{*splat}", "/jobs"));
        assert!(!catch_all_covers_call_literal("/v1/*rest", "/v1/:id"));
        assert!(!catch_all_covers_call_literal("/v1/*rest", "/v1"));
    }

    /// #537: the consumer-side gap. A call whose path never resolved to a
    /// literal segment names no operation, whatever it looks like.
    #[test]
    fn unknown_call_paths_are_the_ones_with_no_literal_segment() {
        assert!(is_unknown_call_path(""));
        assert!(is_unknown_call_path("/"));
        assert!(is_unknown_call_path("*"));
        assert!(is_unknown_call_path("/*"));
        assert!(is_unknown_call_path("/**"));
        assert!(is_unknown_call_path("/(.*)"));
        // A template-literal URL whose every segment interpolated.
        assert!(is_unknown_call_path("/:base/:rest"));
        assert!(is_unknown_call_path("   "));

        assert!(!is_unknown_call_path("/api"));
        assert!(!is_unknown_call_path("/api/v1/auth/universal-auth/login"));
        assert!(!is_unknown_call_path("/users/:id"));
    }

    /// #537 defect 2, both halves: an unknown consumer path never matches
    /// anything, and a bare catch-all producer never absorbs a call. Each
    /// rejection names itself.
    #[test]
    fn unknown_call_path_and_bare_catch_all_are_never_reportable() {
        // Wildcard on both sides — the exact-equality shortcut used to score
        // this Some(0), which reads as a match to anything that only asks
        // `paths_match`.
        assert_eq!(match_verdict("/*", "/*"), MatchVerdict::UnknownCallPath);
        assert_eq!(reportable_agreement("/*", "/*"), None);

        // Unknown call path against a perfectly concrete route.
        assert_eq!(
            match_verdict("/api/v1/auth/login", "/*"),
            MatchVerdict::UnknownCallPath
        );
        assert_eq!(reportable_agreement("/api/v1/auth/login", "/*"), None);

        // Bare catch-all producer against a concrete call: the call is routed
        // by the fallback but corroborated by nothing.
        assert_eq!(
            match_verdict("/*", "/api/v1/auth/login"),
            MatchVerdict::CatchAllRoute
        );
        assert_eq!(reportable_agreement("/*", "/api/v1/auth/login"), None);
        assert_eq!(
            match_verdict("/(.*)", "/api/v1/auth/login"),
            MatchVerdict::CatchAllRoute
        );

        // `paths_match` still answers ROUTING truth for both — the split the
        // crate documents is deliberate and must not silently close.
        assert!(paths_match("/*", "/api/v1/auth/login"));
        assert_eq!(match_agreement("/*", "/api/v1/auth/login"), Some(0));
    }

    /// A catch-all with a literal prefix is a real mount, not a fallback: it
    /// still corroborates the segments it names, so it stays reportable.
    #[test]
    fn prefixed_catch_all_and_concrete_routes_stay_reportable() {
        assert_eq!(
            match_verdict("/files/**", "/files/report/pdf"),
            MatchVerdict::Matched
        );
        assert_eq!(
            reportable_agreement("/files/**", "/files/report/pdf"),
            Some(1)
        );

        assert_eq!(
            match_verdict("/users/:id", "/users/123"),
            MatchVerdict::Matched
        );
        assert_eq!(reportable_agreement("/users/:id", "/users/123"), Some(1));
    }

    /// The two non-degenerate rejections keep their own names, so a surface
    /// can tell "nothing serves this" apart from "something covers it but
    /// vouches for nothing".
    #[test]
    fn no_route_match_and_no_literal_agreement_are_distinct() {
        assert_eq!(
            match_verdict("/api/orders", "/api/invoices"),
            MatchVerdict::NoRouteMatch
        );
        // Param-only route covering a concrete one-segment call.
        assert_eq!(
            match_verdict("/:slug", "/about"),
            MatchVerdict::NoLiteralAgreement
        );
        assert_eq!(reportable_agreement("/:slug", "/about"), None);
    }
}
